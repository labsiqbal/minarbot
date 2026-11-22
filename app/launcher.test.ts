import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFileSync, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { randomUUID } from 'node:crypto';
import { chmodSync, copyFileSync, existsSync, linkSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { createServer, createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { testDatabase } from './test-support/database.ts';
import { nativeReleaseFixture } from './test-support/native-release-fixture.ts';
import { launcherCommand } from './rust-test-launcher.ts';
import { rustBinary } from './rust-binary.ts';
import { fileManifest, selectRelease } from './test-support/release.ts';
import { launcherRequest } from './rust-launcher-test.ts';

const delay = (ms: number) => new Promise(done => setTimeout(done, ms));
async function until<T>(read: () => Promise<T | false>, message = 'Launcher test timed out'): Promise<T> {
  for (let i = 0; i < 150; i++) { try { const result = await read(); if (result) return result; } catch {} await delay(50); }
  throw new Error(message);
}
async function stop(child: ChildProcess) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, 'exit'); child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 6000);
  try { await exited; } finally { clearTimeout(timer); }
}
async function fixture(longHome = false) {
  const dir = mkdtempSync(join(tmpdir(), 'launcher-contract-'));
  const home = longHome ? join(dir, 'long-home-'.repeat(18)) : dir;
  const data = join(home, 'data'), releases = join(home, 'releases');
  mkdirSync(data, { recursive: true }); mkdirSync(releases);
  const server = createServer(); await new Promise<void>(done => server.listen(0, '127.0.0.1', done));
  const address = server.address(); assert(address && typeof address !== 'string'); const port = address.port;
  await new Promise<void>(done => server.close(() => done()));
  const config = join(dir, 'launcher.json'), settings = { dataDir: data, releasesDir: releases, port, trialTimeoutMs: 1000, envFile: join(dir, 'env') };
  writeFileSync(settings.envFile, '', { mode: 0o600 });
  const save = () => writeFileSync(config, JSON.stringify(settings), { mode: 0o600 }); save();
  const options = { launcherSocket: join(data, 'launcher.sock'), releasesDir: releases };
  const db = testDatabase(join(data, 'minarbot.sqlite')); db.exec("CREATE TABLE data(value TEXT); INSERT INTO data VALUES('preserved')"); db.close();
  const processes: ChildProcess[] = [];
  const publish = (version: string, mode?: string) => {
    const id = version + '-12345678', release = join(releases, id); mkdirSync(release);
    writeFileSync(join(release, 'package.json'), JSON.stringify({ version, type: 'module' }));
    nativeReleaseFixture(release, version, mode);
    const seal = () => writeFileSync(join(release, 'release.json'), JSON.stringify({ id, version, files: fileManifest(release) })); seal();
    return { id, version, release, seal };
  };
  const start = () => {
    const child = spawn(...launcherCommand(config), { env: { ...process.env, HOME: home }, stdio: ['ignore', 'pipe', 'pipe'] }); processes.push(child);
    let log = ''; child.stdout!.on('data', b => { log += b; }); child.stderr!.on('data', b => { log += b; });
    return { child, log: () => log };
  };
  return { dir, data, releases, config, settings, options, save, publish, start, read: async () => (await fetch(`http://127.0.0.1:${port}`, { signal: AbortSignal.timeout(500) })).json(), async close() { await Promise.all(processes.map(stop)); rmSync(dir, { recursive: true, force: true }); } };
}
async function rejectsStart(f: Awaited<ReturnType<typeof fixture>>, pattern: RegExp) {
  const { child, log } = f.start(), timer = setTimeout(() => child.kill('SIGKILL'), 3000);
  try { const [code] = await once(child, 'exit'); assert.notEqual(code, 0, log()); assert.match(log(), pattern); }
  finally { clearTimeout(timer); }
}

test('launcher starts and accepts update control with a long HOME', async () => {
  const f = await fixture(true);
  try {
    const first = f.publish('1.0.0'); selectRelease(f.releases, 'current', first.id);
    const running = f.start();
    const socket = await until(async () => (await f.read()).env.MINARBOT_LAUNCHER_SOCKET, 'Launcher did not serve with a long HOME').catch(error => {
      throw new Error(`${error.message}\n${running.log()}`);
    });
    assert.equal(statSync(socket).mode & 0o777, 0o600);
    assert.equal(statSync(join(socket, '..')).mode & 0o777, 0o700);
    assert.equal((await launcherRequest({ ...f.options, launcherSocket: socket }, { type: 'status' })).current, first.id);
    await stop(running.child);
    assert(!existsSync(socket));
  } finally { await f.close(); }
});

test('launcher rejects invalid config, public files, symlinks and hard links', async () => {
  const f = await fixture();
  try {
    for (const invalid of [null, [], { ...f.settings, unknown: true }, { ...f.settings, dataDir: 'relative' }, { ...f.settings, envFile: null }, { ...f.settings, port: 0 }, { ...f.settings, port: 1.5 }, { ...f.settings, trialTimeoutMs: 499 }, { ...f.settings, trialTimeoutMs: 1800001 }]) {
      writeFileSync(f.config, JSON.stringify(invalid)); await rejectsStart(f, /Invalid launcher/);
    }
    f.save(); chmodSync(f.config, 0o640); await rejectsStart(f, /private files/); chmodSync(f.config, 0o600);
    const copy = join(f.dir, 'config-copy'); linkSync(f.config, copy); await rejectsStart(f, /private files/); rmSync(copy);
    rmSync(f.config); writeFileSync(copy, JSON.stringify(f.settings), { mode: 0o600 }); symlinkSync(copy, f.config); await rejectsStart(f, /private files/);
    rmSync(f.config); f.save(); chmodSync(f.settings.envFile, 0o644); await rejectsStart(f, /private files/);
  } finally { await f.close(); }
});

test('launcher parses literal env, overrides routing and delivers native socket commit; receipts survive restart', async () => {
  const f = await fixture();
  try {
    const first = f.publish('1.0.0'); selectRelease(f.releases, 'current', first.id);
    const canary = join(f.dir, 'must-not-exist');
    writeFileSync(f.settings.envFile, `export LAUNCHER_QUOTED="line one\nline two"\nLAUNCHER_LITERAL=$(touch ${canary})\nLAUNCHER_SINGLE='literal # value'\nLAUNCHER_EMPTY=\nPORT=1\nMINARBOT_DATA_DIR=/wrong\nMINARBOT_UPDATE_TRIAL=untrusted\n`);
    let process = f.start(); const initial = await until(async () => await f.read());
    assert.equal(initial.connected, true); assert.equal(initial.env.LAUNCHER_QUOTED, 'line one\nline two');
    assert.equal(initial.env.LAUNCHER_SINGLE, 'literal # value'); assert.equal(initial.env.LAUNCHER_EMPTY, '');
    assert.equal(initial.env.LAUNCHER_LITERAL, `$(touch ${canary})`); assert(!existsSync(canary));
    assert.equal(initial.env.PORT, String(f.settings.port)); assert.equal(initial.env.MINARBOT_DATA_DIR, f.data); assert.equal(initial.env.MINARBOT_UPDATE_TRIAL, undefined);
    const next = f.publish('1.0.1'), request = { type: 'apply', id: randomUUID(), target: next.id, version: next.version };
    await launcherRequest(f.options, request);
    await until(async () => { const value = await f.read(); return value.committed === request.id && value; });
    const snapshot = join(f.data, '.updates', request.id, 'minarbot.sqlite'); assert.equal(statSync(snapshot).mode & 0o777, 0o600);
    assert.equal(statSync(join(f.data, 'update-state.json')).mode & 0o777, 0o600); assert.equal(statSync(f.options.launcherSocket).mode & 0o777, 0o600);
    await stop(process.child); process = f.start(); await until(async () => (await f.read()).version === next.version);
    assert.equal((await launcherRequest(f.options, request)).accepted, true);
    await assert.rejects(launcherRequest(f.options, { ...request, version: '9.0.0' }), /already used/);
  } finally { await f.close(); }
});

test('launcher rejects corrupt releases and strict socket requests without stopping current server', async () => {
  const f = await fixture();
  try {
    const first = f.publish('1.0.0'); selectRelease(f.releases, 'current', first.id); f.start(); await until(async () => await f.read());
    for (const request of [{ type: 'status', extra: 1 }, { type: 'other' }, { type: 'apply', id: 'bad', target: first.id, version: first.version }]) await assert.rejects(launcherRequest(f.options, request), /control request|update request/);
    const bad = f.publish('1.0.1'); writeFileSync(join(bad.release, 'bin/minarbot-server'), 'tampered');
    await assert.rejects(launcherRequest(f.options, { type: 'apply', id: randomUUID(), target: bad.id, version: bad.version }), /manifest mismatch/);
    const oversized = createConnection(f.options.launcherSocket); oversized.on('error', () => {}); const closed = once(oversized, 'close'); oversized.write('x'.repeat(4097)); await closed;
    assert.equal((await launcherRequest(f.options, { type: 'status' })).current, first.id);
    assert.equal((await f.read()).version, first.version);
  } finally { await f.close(); }
});

test('launcher rejects tampered startup and public persisted state', async () => {
  const f = await fixture();
  try {
    const first = f.publish('1.0.0'); selectRelease(f.releases, 'current', first.id);
    writeFileSync(join(first.release, 'bin/minarbot-server'), 'tampered'); await rejectsStart(f, /manifest mismatch/);
    const state = join(f.data, 'update-state.json'); chmodSync(state, 0o644); await rejectsStart(f, /private files/);
  } finally { await f.close(); }
});

test('launcher commits a native trial after its first health probe disconnects', async () => {
  const f = await fixture();
  try {
    f.settings.trialTimeoutMs = 15_000;
    f.save();
    const first = f.publish('1.0.0');
    selectRelease(f.releases, 'current', first.id);
    f.start();
    await until(f.read);

    const next = f.publish('1.0.1', 'expired-probe'), id = randomUUID();
    await launcherRequest(f.options, { type: 'apply', id, target: next.id, version: next.version });
    const attempt = await until(async () => {
      const { lastAttempt } = await launcherRequest(f.options, { type: 'status' });
      const value = lastAttempt as { status: string; reason?: string };
      return ['done', 'rolled-back', 'failed'].includes(value.status) && value;
    });
    assert.equal(attempt.status, 'done', attempt.reason);
    const candidate = await f.read();
    assert.equal(candidate.version, next.version);
    assert.equal(candidate.committed, id);
  } finally { await f.close(); }
});

test('Rust launcher selects native release, checks chunked health, snapshots, and stops orphan on parent death', async () => {
  const f = await fixture();
  try {
    const first = f.publish('1.0.0'); selectRelease(f.releases, 'current', first.id); const process = f.start(); await until(async () => await f.read());
    const next = f.publish('1.0.1'); 
    execFileSync('rustc', [fileURLToPath(new URL('../crates/minarbot-launcher/tests/fixtures/server.rs', import.meta.url)), '-o', join(next.release, 'bin/minarbot-server')]);
    writeFileSync(join(next.release, 'version.txt'), next.version); writeFileSync(join(next.release, 'server.ts'), "throw new Error('Node fallback must not run')"); next.seal();
    await launcherRequest(f.options, { type: 'apply', id: randomUUID(), target: next.id, version: next.version });
    await until(async () => ((await launcherRequest(f.options, { type: 'status' })).lastAttempt as { status?: string } | undefined)?.status === 'done');
    assert.equal((await f.read()).native, true);
    const died = once(process.child, 'exit'); process.child.kill('SIGKILL'); await died;
    await until(async () => { try { await f.read(); return false; } catch { return true; } });
  } finally { await f.close(); }
});

test('launcher updates and snapshots after its own release binary was deleted', async () => {
  const f = await fixture();
  // A long-running launcher keeps the binary it started from; release cleanup can delete that file.
  const copy = join(f.dir, 'launcher-copy');
  copyFileSync(rustBinary('minarbot-launcher', process.env.MINARBOT_RUST_LAUNCHER_BINARY), copy); chmodSync(copy, 0o700);
  const first = f.publish('1.0.0'); selectRelease(f.releases, 'current', first.id);
  const child = spawn(copy, ['--config', f.config], { stdio: ['ignore', 'pipe', 'pipe'] });
  let log = ''; child.stdout!.on('data', b => { log += b; }); child.stderr!.on('data', b => { log += b; });
  try {
    await until(async () => await f.read());
    rmSync(copy);
    const next = f.publish('1.0.1');
    execFileSync('rustc', [fileURLToPath(new URL('../crates/minarbot-launcher/tests/fixtures/server.rs', import.meta.url)), '-o', join(next.release, 'bin/minarbot-server')]);
    writeFileSync(join(next.release, 'version.txt'), next.version); next.seal();
    await launcherRequest(f.options, { type: 'apply', id: randomUUID(), target: next.id, version: next.version });
    const attempt = await until(async () => {
      const value = (await launcherRequest(f.options, { type: 'status' })).lastAttempt as { status?: string; reason?: string } | undefined;
      return value && ['done', 'rolled-back', 'failed'].includes(value.status ?? '') ? value : false;
    });
    assert.equal(attempt.status, 'done', `${attempt.reason ?? ''}\n${log}`);
    assert.equal((await f.read()).native, true);
  } finally { await stop(child); await f.close(); }
});
