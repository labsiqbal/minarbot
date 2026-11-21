// Adapted from T3 updateEverything.logic.test.ts (MIT). See app/t3/provenance.json.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { readJson } from './src/http.ts';
import { describe, it } from 'node:test';
import type { PairedMachine } from './shared/machines.ts';
import type { UpdateAttempt, UpdateState } from './shared/updates.ts';
import {
  availableUpdates, collectServerUpdateTargets, describeUpdateCapsule, describeUpdateEverything, downloadLabel, nextUpdateCommand,
  planUpdateRun, resolveUpdateRunMachineStatus, type UpdateMachine,
} from './src/updateEverything.logic.ts';
import { readSavedUpdateRun, submitUpdateCommand, updateRunKey, withUpdateReceipts } from './src/updateEverythingRun.ts';

const release = (version: string) => ({ id: version, version, notes: '', commit: '', created: '' });
const state: UpdateState = { current: release('0.12.3'), available: release('0.13.0'), launcherAvailable: true, runningTurns: 2 };
const peer = (id: string, overrides: Partial<PairedMachine> = {}): PairedMachine => ({
  id, name: id, environmentId: id, origin: '', status: 'online', pairingState: 'paired', agentCount: 0,
  agents: [], allowUpdates: true, allowAgentMessaging: false, version: '0.12.3', lastSeen: null, ...overrides,
});
const machine = (id: string, running = 0): UpdateMachine => ({
  id, name: id, current: '0.12.3', target: '0.13.0', running, local: id === 'local', skipped: null,
});
const attempt = (id: string, status: string, reason?: string): UpdateAttempt => ({
  id, status, version: '0.13.0', target: '0.13.0-fixture', reason, started: '',
});

describe('collectServerUpdateTargets', () => {
  it('updates eligible peers and names skipped machines while connected server is already up to date', () => {
    const ids = ['ready', 'manual', 'offline', 'stale', 'disabled', 'current', 'ahead', 'busy'];
    const rows = collectServerUpdateTargets({ ...state, available: undefined, feed: state.available }, [
      peer('ready'), peer('manual'), peer('offline', { status: 'offline' }), peer('stale', { status: 'stale' }), peer('disabled', { allowUpdates: false }),
      peer('current', { version: '0.13.0' }), peer('ahead', { version: '0.14.0' }), peer('busy'),
    ], ids.map(id => ({ machine: id, available: true, launcherAvailable: id !== 'manual', ...(id === 'busy'
      ? { lastAttempt: attempt('other-run', 'starting') } : {}) })), 'Studio');
    assert.deepEqual(rows.map(row => [row.name, row.skipped]), [
      ['Studio', 'Up to date'], ['ready', null], ['manual', 'Updates unavailable'], ['offline', 'Offline'],
      ['stale', 'Status stale'], ['disabled', 'Remote updates off'], ['current', 'Up to date'], ['ahead', 'Up to date'], ['busy', 'Already updating'],
    ]);
  });
});

it('offers both update sources and keeps the preferred GitHub source on peer and local commands', () => {
  const github = { ...release('0.13.0'), source: 'GitHub' as const };
  const feed = { ...release('0.12.9'), source: 'Local feed' as const };
  const snapshot = { ...state, available: github, github, feed };
  assert.deepEqual(availableUpdates(snapshot), [github, feed]);
  const rows = collectServerUpdateTargets(snapshot, [peer('peer')], [{ machine: 'peer', available: true, launcherAvailable: true }], 'Studio');
  const run = planUpdateRun(rows, () => crypto.randomUUID());
  assert.deepEqual(run.commands.map(({ version, source }) => [version, source]), [['0.13.0', 'GitHub'], ['0.13.0', 'GitHub']]);
  assert.deepEqual(readSavedUpdateRun({ getItem: () => JSON.stringify(run) }), run);
  assert.equal(downloadLabel({ bytesTransferred: 512, totalBytes: 2048 }), 'Downloading 25%');
});

describe('describeUpdateEverything', () => {
  it('counts the running turns on restarting machines only and keeps skipped rows', () => {
    const rows = [machine('local', 2), machine('peer', 1), { ...machine('offline', 9), skipped: 'Offline' }];
    assert.deepEqual(describeUpdateEverything(rows), {
      count: 2, version: '0.13.0', warning: '3 running turns will be interrupted and can resume after the restart.',
    });
    assert.match(describeUpdateEverything([machine('peer', 1)]).warning, /^1 running turn will /);
  });
});

describe('update run', () => {
  it('batches every eligible pair and keeps the connected server last', () => {
    const rows = [machine('local'), ...Array.from({ length: 17 }, (_, index) => machine(`peer-${index}`))];
    let id = 0;
    const run = planUpdateRun(rows, () => String(++id));
    assert.deepEqual(run.commands.map(command => command.machines.length), [16, 1, 1]);
    assert.deepEqual(run.commands.at(-1)?.machines, ['local']);
    assert.equal(nextUpdateCommand(run)?.id, '1');
  });

  it('keeps the same command and follows the matching remote receipt after reload', () => {
    const run = planUpdateRun([machine('local'), machine('peer')], () => crypto.randomUUID());
    const command = { ...run.commands[0], startedAt: 1 };
    const submitted = withUpdateReceipts(run, command, [{ machine: 'peer', status: 'accepted', id: 'receipt' }]);
    const storage = { getItem: (key: string) => key === updateRunKey ? JSON.stringify(submitted) : null } as Storage;
    const saved = readSavedUpdateRun(storage)!;
    assert.deepEqual(saved, JSON.parse(JSON.stringify(submitted)));
    const remote = saved.rows[1];
    assert.equal(resolveUpdateRunMachineStatus(remote, command, { lastAttempt: attempt('unrelated', 'failed') }, 2), remote);
    const reconnecting = resolveUpdateRunMachineStatus(remote, saved.commands[0], undefined, 2);
    assert.equal(reconnecting.status, 'reconnecting');
    saved.rows[1] = resolveUpdateRunMachineStatus(reconnecting, command,
      { lastAttempt: attempt('receipt', 'rolled-back', 'Health check timed out.') }, 3);
    assert.equal(saved.rows[1].reason, 'Health check timed out.');
    assert.equal(nextUpdateCommand(saved)?.id, run.commands[1].id);
  });

  it('distinguishes a settled result, reconnecting and running progress', () => {
    const run = planUpdateRun([machine('local'), machine('peer')], () => crypto.randomUUID());
    run.rows[1].status = 'done';
    assert.equal(describeUpdateCapsule(run, false).label, '1 of 2 updated');
    assert.equal(describeUpdateCapsule(run, true).label, 'Reconnecting…');
    run.rows[0].status = 'failed';
    assert.deepEqual(describeUpdateCapsule(run, false), { label: '1 update failed', attention: true });
    run.rows[0].status = 'unconfirmed';
    assert.deepEqual(describeUpdateCapsule(run, false), { label: '1 update unconfirmed', attention: true });
    run.rows[0].status = 'done';
    assert.deepEqual(describeUpdateCapsule(run, false), { label: '2 of 2 updated', attention: false });
  });
});

it('sends the existing HTTP bodies and waits for the peer receipt before applying here', async t => {
  const requests: { path: string; body: unknown }[] = [];
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    requests.push({ path: req.url!, body });
    res.writeHead(202, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(req.url === '/api/updates/fan-out'
      ? { machines: [{ machine: 'peer', status: 'accepted', id: 'receipt' }] } : { id: body.id }));
  });
  server.listen(0, '127.0.0.1');
  t.after(() => server.close());
  await once(server, 'listening');
  const address = server.address();
  assert(address && typeof address === 'object');
  const request = async <T>(path: string, body?: unknown) => readJson<T>(await fetch(`http://127.0.0.1:${address.port}${path}`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
  }));
  let run = planUpdateRun([machine('local'), machine('peer')], () => crypto.randomUUID());
  const remote = { ...run.commands[0], startedAt: 1 }, local = run.commands[1];
  run = withUpdateReceipts(run, remote, await submitUpdateCommand(request, remote));
  assert.equal(nextUpdateCommand(run)?.id, remote.id);
  run.rows[1] = resolveUpdateRunMachineStatus(run.rows[1], run.commands[0], { lastAttempt: attempt('receipt', 'done') }, 2);
  assert.equal(nextUpdateCommand(run)?.id, local.id);
  await submitUpdateCommand(request, local);
  assert.deepEqual(requests, [
    { path: '/api/updates/fan-out', body: { id: remote.id, version: '0.13.0', machines: ['peer'] } },
    { path: '/api/updates/apply', body: { id: local.id, version: '0.13.0' } },
  ]);
});
