import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, utimesSync, writeFileSync } from 'node:fs';
import { createServer, request } from 'node:http';
import { createServer as controlServer } from 'node:net';
import { join } from 'node:path';
import { startApp } from './test-bridge.ts';
import { fileManifest, verifyRelease } from './test-support/release.ts';

async function api(origin: string, path: string, body?: unknown) {
  const response = await fetch(origin + path, body === undefined ? {} : { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  const value = await response.json(); assert(response.ok, JSON.stringify({ status: response.status, value })); return value;
}

// The peer runs the current server with its update capabilities hidden, so the sender takes the
// synchronous apply path that releases before peerApplyPolling still use.
test('fan-out serves one verified executable archive to a peer without apply polling and waits beyond its former apply deadline', { timeout: 170000 }, async t => {
  const cleanup: Array<() => unknown> = [];
  t.after(async () => { for (const close of cleanup.reverse()) await close(); });
  const dir = mkdtempSync('/dev/shm/minarbot-fanout-103-');
  cleanup.push(() => rmSync(dir, { recursive: true, force: true }));
  const releases = join(dir, 'releases'), id = '0.11.0-7d5216a6', release = join(releases, id);
  mkdirSync(join(release, 'bin'), { recursive: true }); mkdirSync(join(releases, 'feed'));
  writeFileSync(join(release, 'package.json'), '{"version":"0.11.0"}');
  for (const name of ['minarbot-server', 'minarbot-launcher']) writeFileSync(join(release, 'bin', name), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  const info = { id, version: '0.11.0', commit: '7d5216a6' + '0'.repeat(32), created: '2026-11-18T00:00:00Z', node: process.versions.node, notes: 'Fixture', files: fileManifest(release) };
  for (const path of [join(release, 'release.json'), join(releases, 'feed/latest.json')]) writeFileSync(path, JSON.stringify(info));
  let busy = false, applied = 0;
  const socket = join(dir, 'control.sock'), launcher = controlServer(client => {
    let raw = ''; client.on('data', chunk => {
      raw += chunk; if (!raw.includes('\n')) return;
      const command = JSON.parse(raw);
      if (command.type === 'status') client.end('{"protocol":1,"current":"development"}\n');
      else if (busy) client.end('{"error":"Another update is already running."}\n');
      else { applied++; client.end(JSON.stringify({ accepted: true, id: command.id }) + '\n'); }
    });
  });
  await new Promise<void>(done => launcher.listen(socket, done)); chmodSync(socket, 0o600);
  cleanup.push(() => new Promise<void>(done => launcher.close(() => done())));
  // Pair with the peer's own origin, then move the peer and put the proxy on that origin.
  const peerConfig = { database: join(dir, 'peer.sqlite'), updates: { releasesDir: join(dir, 'received'), launcherSocket: socket } };
  let peer = await startApp(peerConfig);
  const origin = peer.origin;
  const sender = await startApp({ database: join(dir, 'sender.sqlite'), updates: { releasesDir: releases } }); cleanup.push(() => sender.close());
  const code = await api(origin, '/api/pairing-codes', { id: randomUUID() });
  const machine = await api(sender.origin, '/api/machines', { id: randomUUID(), origin, code: code.code });
  const back = (await api(origin, '/api/machines')).machines[0];
  for (const [owner, environmentId] of [[sender.origin, machine.id], [origin, back.id]]) await api(owner, '/api/machines/updates', { id: randomUUID(), environmentId, allowed: true });
  await peer.close();
  peer = await startApp(peerConfig);
  cleanup.push(() => peer.close());
  const peerOrigin = peer.origin;
  let hold = false, mutate = true, untrusted = false;
  const responses: { status: number; body: string }[] = [], timers = new Set<ReturnType<typeof setTimeout>>();
  const proxy = createServer((req, res) => {
    const apply = req.url === '/api/peer/updates/apply';
    if (apply && mutate) { mutate = false; utimesSync(join(release, 'bin/minarbot-server'), 1, 1); }
    const upstream = request(peerOrigin + req.url, { method: req.method, headers: { ...req.headers, host: new URL(peerOrigin).host } }, reply => {
      if (req.url === '/api/environment' || req.url === '/api/peer/updates') {
        const chunks: Buffer[] = []; reply.on('data', bytes => chunks.push(bytes)); reply.on('end', () => {
          const status = JSON.parse(Buffer.concat(chunks).toString() || '{}');
          if (req.url === '/api/environment') status.origin = origin;
          else { delete status.peerApplyPolling; delete status.peerGitHubUpdates; }
          res.writeHead(reply.statusCode!, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(status));
        });
        return;
      }
      if (!apply) { res.writeHead(reply.statusCode!, reply.headers); reply.pipe(res); return; }
      const chunks: Buffer[] = []; reply.on('data', bytes => chunks.push(bytes)); reply.on('end', () => {
        const body = Buffer.concat(chunks).toString(); responses.push({ status: reply.statusCode!, body });
        const send = () => { res.writeHead(reply.statusCode!, { 'Content-Type': 'application/json' }); res.end(untrusted ? '{"error":"fixture-private-value"}' : body); };
        if (hold) { const timer = setTimeout(() => { timers.delete(timer); send(); }, 125000); timers.add(timer); } else send();
      });
    });
    upstream.on('error', () => { res.writeHead(502).end(); }); req.pipe(upstream);
  });
  await new Promise<void>(done => proxy.listen(Number(new URL(origin).port), '127.0.0.1', done));
  cleanup.push(() => { for (const timer of timers) clearTimeout(timer); proxy.closeAllConnections(); return new Promise<void>(done => proxy.close(() => done())); });
  const command = () => ({ id: randomUUID(), machines: [machine.id], version: info.version });
  const first = command(), result = await api(sender.origin, '/api/updates/fan-out', first);
  assert.equal(result.machines[0].status, 'accepted', JSON.stringify({ result, responses }));
  assert.equal(responses[0].status, 202); assert.equal(verifyRelease(join(dir, 'received'), id).version, '0.11.0');
  assert(statSync(join(dir, 'received', id, 'bin/minarbot-server')).mode & 0o111);
  assert.deepEqual(readFileSync(join(dir, 'received', id, 'bin/minarbot-server')), readFileSync(join(release, 'bin/minarbot-server')));
  assert.deepEqual(await api(sender.origin, '/api/updates/fan-out', first), result); assert.equal(applied, 1);
  busy = true;
  const rejected = await api(sender.origin, '/api/updates/fan-out', command());
  assert.equal(responses.at(-1)?.status, 409);
  assert.equal(JSON.parse(responses.at(-1)!.body).error, 'Another update is already running.');
  assert.equal(rejected.machines[0].reason, 'Remote machine rejected the update (HTTP 409). Another update is already running.');
  untrusted = true;
  const privateError = await api(sender.origin, '/api/updates/fan-out', command());
  assert.equal(privateError.machines[0].reason, 'Remote machine rejected the update (HTTP 409).');
  untrusted = false; busy = false; hold = true;
  const started = Date.now(), slow = await api(sender.origin, '/api/updates/fan-out', command());
  assert(Date.now() - started >= 120000); assert.equal(slow.machines[0].status, 'accepted', JSON.stringify(slow));
  assert.equal(responses.at(-1)?.status, 202); assert.equal(applied, 2);
  assert(!readdirSync(releases).some(name => name.startsWith('.archive-')));
});
