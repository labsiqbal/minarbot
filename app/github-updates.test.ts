import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { once } from 'node:events';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer, request, type RequestListener } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test, type TestContext } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { fileManifest, selectRelease, selectedRelease } from '../scripts/release-lib.mjs';
import { rustBinary } from './rust-binary.ts';
import { launcherCommand } from './rust-test-launcher.ts';
import { startApp } from './test-bridge.ts';
import { nativeReleaseFixture } from './test-support/native-release-fixture.ts';
import { freePort } from '../scripts/test-port.mjs';

const base = '/labsiqbal/minarbot/releases';
const created = '2026-11-19T09:00:00.000Z';
const currentVersion = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')).version;
const version = '9.1.2';
const commit = 'abcdef1234567890abcdef1234567890abcdef12';
const archive = `minarbot-${version}-linux-x86_64.tar.gz`;
const hash = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');

async function listen(t: TestContext, handler?: RequestListener, port = 0) {
  const server = createServer(handler);
  server.listen(port, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  assert(address && typeof address === 'object');
  const close = async () => {
    if (!server.listening) return;
    server.closeAllConnections();
    await new Promise<void>(resolve => server.close(() => resolve()));
  };
  cleanup(t, close);
  return { origin: `http://127.0.0.1:${address.port}`, close };
}

const cleanups = new WeakMap<TestContext, (() => void | Promise<void>)[]>();
function cleanup(t: TestContext, close: () => void | Promise<void>) {
  const actions = cleanups.get(t) ?? [];
  if (!cleanups.has(t)) {
    cleanups.set(t, actions);
    t.after(async () => {
      for (const close of actions.reverse()) await close();
    });
  }
  actions.push(close);
}
function directory(t: TestContext) {
  const path = mkdtempSync(join(tmpdir(), 'minarbot-github-'));
  cleanup(t, () => rmSync(path, { recursive: true, force: true }));
  return path;
}

function release(root: string, version: string, binary?: string) {
  const id = version + '-' + commit.slice(0, 8), dir = join(root, id);
  mkdirSync(join(dir, 'bin'), { recursive: true });
  if (binary) writeFileSync(join(dir, 'bin/minarbot-server'), binary, { mode: 0o700 });
  else nativeReleaseFixture(dir, version);
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ version }));
  const info = { id, version, commit, created, notes: 'Signed fixture', node: process.version, files: fileManifest(dir) };
  writeFileSync(join(dir, 'release.json'), JSON.stringify(info));
  return info;
}

type Fault = 'signature' | 'missing-signature' | 'archive-commit' | 'hash' | 'truncated' | 'redirect' | 'unknown' | 'commit' | 'version';
async function github(t: TestContext, root: string, target: ReturnType<typeof release>, fault?: Fault, resume = false) {
  const file = join(root, archive);
  execFileSync('tar', ['--format=ustar', '-czf', file, '-C', join(root, target.id), '.']);
  const bytes = readFileSync(file), { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const manifest = Buffer.from(JSON.stringify({
    id: target.id, version: fault === 'version' ? '9.1.3' : version,
    commit: fault === 'commit' ? 'invalid' : fault === 'archive-commit' ? commit.slice(0, -1) + '0' : commit, archive, created, notes: target.notes,
    sha256: fault === 'hash' ? '0'.repeat(64) : hash(bytes), size: bytes.length,
    ...(resume ? { assets: { [archive]: { sha256: hash(bytes), size: bytes.length } } } : {}),
    ...(fault === 'unknown' ? { extra: true } : {}),
  }, null, 2) + '\n');
  const signature = sign(null, fault === 'signature' ? manifest.subarray(0, -1) : manifest, privateKey).toString('base64');
  const ranges: (string | undefined)[] = [];
  let interrupt: (() => void) | undefined;
  const cdn = await listen(t, (req, res) => {
    assert.equal(req.url, '/asset');
    ranges.push(req.headers.range);
    if (fault === 'truncated') return res.end(bytes.subarray(0, -1));
    if (resume && ranges.length === 1) {
      res.writeHead(200, { 'Content-Length': bytes.length });
      interrupt = () => res.destroy();
      res.write(bytes.subarray(0, 64));
      return;
    }
    const offset = Number(req.headers.range?.match(/^bytes=(\d+)-$/)?.[1] ?? 0);
    if (offset) res.writeHead(206, { 'Content-Range': `bytes ${offset}-${bytes.length - 1}/${bytes.length}`, 'Content-Length': bytes.length - offset });
    res.end(bytes.subarray(offset));
  });
  const origin = await listen(t, (req, res) => {
    if (req.url === base + '/latest/download/latest.json') return res.end(manifest);
    if (req.url === base + '/latest/download/latest.json.sig' && fault !== 'missing-signature') return res.end(signature);
    if (req.url === base + `/download/v${version}/${archive}`) {
      res.writeHead(302, { Location: fault === 'redirect' ? cdn.origin.replace('127.0.0.1', '127.0.0.2') + '/asset' : cdn.origin + '/asset' });
      return res.end();
    }
    res.writeHead(404).end();
  });
  return {
    options: { testReleaseBaseUrl: origin.origin + base, testReleasePublicKey: publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('base64') },
    ranges, size: bytes.length, close: origin.close,
    resume() { assert(interrupt); interrupt(); },
  };
}

async function api(origin: string, path: string, body?: unknown, status = 200, timeout = 15_000) {
  const response = await fetch(origin + path, body === undefined ? { signal: AbortSignal.timeout(timeout) } : {
    method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' },
    body: JSON.stringify(body), signal: AbortSignal.timeout(timeout),
  });
  const value = await response.json();
  assert.equal(response.status, status, JSON.stringify(value));
  return value;
}

async function until<T>(probe: () => Promise<T | undefined>): Promise<T> {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const result = await probe();
    if (result !== undefined) return result;
    await delay(70);
  }
  assert.fail('Timed out waiting for update');
}

async function managed(t: TestContext, path: string, updates: object = {}) {
  const data = join(path, 'data'), releases = join(path, 'releases');
  mkdirSync(data, { recursive: true, mode: 0o700 });
  const port = await freePort();
  const probe = { origin: `http://127.0.0.1:${port}` };
  const launcherSocket = join(data, 'launcher.sock'), database = join(data, 'minarbot.sqlite');
  const config = join(path, 'server.json');
  writeFileSync(config, JSON.stringify({ database, port, updates: { ...updates, releasesDir: releases, launcherSocket } }), { mode: 0o600 });
  const binary = rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY);
  const wrapper = `#!/bin/sh\nexec '${binary}' --test-config < '${config}'\n`;
  const current = release(releases, currentVersion, wrapper);
  selectRelease(releases, 'current', current.id);
  const launcherConfig = join(path, 'launcher.json');
  writeFileSync(launcherConfig, JSON.stringify({ dataDir: data, releasesDir: releases, port, trialTimeoutMs: 15_000 }), { mode: 0o600 });
  const child = spawn(...launcherCommand(launcherConfig), {
    env: { ...process.env, HOME: path, MINARBOT_TEST_SERVER: 'rust', MINARBOT_TEST_PARENT: String(process.pid) },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  let logs = '';
  child.stderr.on('data', bytes => { logs += bytes; });
  const exited = once(child, 'exit');
  cleanup(t, async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill();
    await exited;
  });
  await until(async () => {
    assert.equal(child.exitCode, null, logs);
    if (!existsSync(launcherSocket)) return undefined;
    return fetch(probe.origin + '/api/environment').then(response => response.ok || undefined).catch(() => undefined);
  });
  return { origin: probe.origin, releases, data, current, get logs() { return logs; } };
}

const check = '/api/updates', apply = '/api/updates/apply';
test('signed GitHub release resumes through the CDN and completes a real launcher trial', async t => {
  const dir = directory(t);
  const staged = join(dir, 'publish');
  const publisher = await github(t, staged, release(staged, version), undefined, true);
  const app = await managed(t, join(dir, 'machine'), publisher.options);
  const state = await api(app.origin, check);
  assert.equal(state.available?.version, version);
  assert.equal(state.available.source, 'GitHub');
  const id = randomUUID(), command = { id, version, source: 'GitHub' };
  assert.equal((await api(app.origin, apply, command, 202)).id, id);
  assert.equal((await api(app.origin, apply, command, 202)).id, id);
  const progress = await until(async () => {
    const attempt = (await api(app.origin, check)).lastAttempt;
    return attempt?.id === id && attempt.bytesTransferred === 64 ? attempt : undefined;
  });
  assert.equal(progress.totalBytes, publisher.size);
  assert.equal(progress.source, 'GitHub');
  publisher.resume();
  await waitForTrial(app, id, state.available.id);
  assert.deepEqual(publisher.ranges, [undefined, 'bytes=64-']);
  assert.equal((await api(app.origin, '/api/environment')).version, version);
  assert(existsSync(join(app.data, '.updates', id, 'minarbot.sqlite')));
});

for (const [fault, message, atCheck] of [
  ['signature', /signature/i, true], ['missing-signature', /signature is missing/i, true],
  ['unknown', /unknown field/i, true],
  ['commit', /commit/i, true], ['version', /version/i, true], ['archive-commit', /does not match the archive/i, false],
  ['hash', /SHA-256/i, false], ['truncated', /truncated|size mismatch/i, false],
  ['redirect', /redirect/i, false],
] as const) {
  test(`GitHub ${fault} fixture is rejected while current release keeps running`, async t => {
    const dir = directory(t);
    const staged = join(dir, 'publish'), target = release(staged, version);
    const publisher = await github(t, staged, target, fault);
    const app = await managed(t, join(dir, 'machine'), publisher.options);
    const state = await api(app.origin, check);
    if (atCheck) {
      assert.equal(state.available, undefined);
      assert.match(state.githubError ?? '', message);
    } else {
      assert.equal(state.available?.version, version);
      const id = randomUUID();
      await api(app.origin, apply, { id, version, source: 'GitHub' }, 202);
      const failure = await until(async () => {
        const status = (await api(app.origin, check)).lastAttempt;
        return status?.id === id && status.status === 'failed' ? status : undefined;
      });
      assert.match(failure.reason, message);
    }
    assert.equal((await api(app.origin, '/api/environment')).version, app.current.version);
    assert.equal(selectedRelease(app.releases), app.current.id);
    assert(!existsSync(join(app.releases, target.id)));
    if (fault === 'redirect') assert.deepEqual(publisher.ranges, []);
  });
}

async function waitForTrial(app: Awaited<ReturnType<typeof managed>>, id: string, target: string) {
  await until(async () => {
    const status = JSON.parse(readFileSync(join(app.data, 'update-state.json'), 'utf8'));
    const attempt = status.lastAttempt;
    if (attempt?.id !== id) return undefined;
    assert(!['failed', 'rolled-back'].includes(attempt.status), attempt.reason ?? app.logs);
    return attempt.status === 'done' || undefined;
  });
  assert.equal(selectedRelease(app.releases), target);
}

// Holds apply replies until the receiver restarted into the new release, so the sender's
// confirmation always comes from the restarted server, as it does on a slow link.
async function lateReplies(t: TestContext, receiver: Awaited<ReturnType<typeof managed>>) {
  let origin = '';
  const committed = () => {
    try { return JSON.parse(readFileSync(join(receiver.data, 'update-state.json'), 'utf8')).lastAttempt?.status === 'done'; }
    catch { return false; }
  };
  ({ origin } = await listen(t, (req, res) => {
    const upstream = request(receiver.origin + req.url, { method: req.method, headers: { ...req.headers, host: new URL(receiver.origin).host } }, async reply => {
      if (req.url === '/api/environment' || req.url === '/api/pairing/redeem') {
        // The receiver identifies itself by the origin the sender paired with.
        const chunks = [];
        for await (const chunk of reply) chunks.push(chunk);
        res.writeHead(reply.statusCode ?? 502, { 'Content-Type': 'application/json' });
        res.end(Buffer.concat(chunks).toString().replaceAll(JSON.stringify(receiver.origin), JSON.stringify(origin)));
      } else if (req.url === '/api/peer/updates/apply' && !committed()) {
        reply.resume();
        res.destroy();
      } else {
        res.writeHead(reply.statusCode ?? 502, reply.headers);
        reply.pipe(res);
      }
    });
    upstream.on('error', () => res.destroy());
    res.on('close', () => upstream.destroy());
    req.pipe(upstream);
  }));
  return origin;
}

for (const route of ['direct', 'late-reply', 'fallback', 'invalid'] as const) {
  test(`GitHub peer fan-out uses ${route} transfer policy`, async t => {
    const dir = directory(t), staged = join(dir, 'publish');
    const target = release(staged, version);
    const publisher = await github(t, staged, target);
    const receiverPublisher = route === 'direct' || route === 'late-reply' ? publisher
      : await github(t, staged, target, route === 'invalid' ? 'signature' : undefined);
    if (route === 'fallback') await receiverPublisher.close();
    const receiver = await managed(t, join(dir, 'receiver'), receiverPublisher.options);
    const receiverOrigin = route === 'late-reply' ? await lateReplies(t, receiver) : receiver.origin;
    const releasesDir = join(dir, 'sender'), database = join(dir, 'sender.sqlite');
    const options = { database, updates: { releasesDir, ...publisher.options } };
    const initial = await startApp(options);
    cleanup(t, initial.close);
    const { code } = await api(receiver.origin, '/api/pairing-codes', { id: randomUUID() });
    const machine = await api(initial.origin, '/api/machines', { id: randomUUID(), origin: receiverOrigin, code });
    const { machines: [sender] } = await api(receiver.origin, '/api/machines');
    await api(initial.origin, '/api/machines/updates', { id: randomUUID(), environmentId: machine.id, allowed: true });
    await api(receiver.origin, '/api/machines/updates', { id: randomUUID(), environmentId: sender.id, allowed: true });
    await initial.close();

    let archiveRequests = 0, archiveBytes = 0, backend: string;
    await listen(t, (req, res) => {
      const upstream = request(backend + req.url, { method: req.method, headers: { ...req.headers, host: new URL(backend).host } }, async reply => {
        if (req.url === '/api/environment') {
          const chunks = [];
          for await (const chunk of reply) chunks.push(chunk);
          const value = JSON.parse(Buffer.concat(chunks).toString());
          res.writeHead(reply.statusCode ?? 502, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ ...value, origin: initial.origin }));
        } else {
          if (req.url?.startsWith('/api/updates/releases/')) {
            archiveRequests++;
            reply.on('data', chunk => { archiveBytes += chunk.length; });
          }
          res.writeHead(reply.statusCode ?? 502, reply.headers);
          reply.pipe(res);
        }
      });
      upstream.on('error', () => res.destroy());
      res.on('close', () => upstream.destroy());
      req.pipe(upstream);
    }, Number(new URL(initial.origin).port));
    const app = await startApp(options);
    cleanup(t, app.close);
    backend = app.origin;

    if (route === 'fallback') {
      const feed = join(receiver.releases, 'feed');
      mkdirSync(feed);
      writeFileSync(join(feed, 'latest.json'), JSON.stringify(release(receiver.releases, '8.0.0')));
      const state = await api(receiver.origin, check);
      assert.match(state.githubError, /GitHub is unreachable/i);
      assert.equal(state.available.source, 'Local feed');
      assert.equal(state.available.version, '8.0.0');
    }
    const id = randomUUID(), command = { id, machines: [machine.id], version, source: 'GitHub' };
    const path = '/api/updates/fan-out';
    // The fan-out reply waits for the peer's transfer and apply, which can exceed 15 s under suite load.
    const result = (await api(app.origin, path, command, 202, 60_000)).machines[0];
    const attempt = async () => (await api(app.origin, '/api/updates/machines')).machines[0]?.fanOutAttempt;
    const final = await attempt();
    if (route === 'invalid') {
      assert.equal(result.status, 'failed');
      assert.match(result.reason, /GitHub release signature/i);
      assert.equal(selectedRelease(receiver.releases), receiver.current.id);
      assert.equal(archiveRequests, 0);
      assert.equal(archiveBytes, 0);
      return;
    }
    assert.equal(result.status, 'accepted', JSON.stringify(result));
    assert.equal(final.source, route === 'fallback' ? 'Local feed' : 'GitHub');
    assert.equal(final.bytesTransferred, final.totalBytes);
    assert.equal((await api(app.origin, path, command, 202)).machines[0].id, result.id);
    await waitForTrial(receiver, result.id, target.id);
    assert.deepEqual(publisher.ranges, [undefined]);
    assert.equal(archiveRequests, route === 'fallback' ? 1 : 0);
    assert.equal(archiveBytes, route === 'fallback' ? final.totalBytes : 0);
    assert.equal(existsSync(join(releasesDir, target.id)), route === 'fallback');
  });
}
