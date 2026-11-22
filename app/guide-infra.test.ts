import assert from 'node:assert/strict';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout } from 'node:timers/promises';
import test from 'node:test';
import { rustServerOutput, startRustServer } from './rust-test-server.ts';

test('Guide infrastructure requires approval and keeps credentials out of replies, storage and logs', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'minarbot-guide-infra-'));
  const bin = join(dir, 'bin'), releases = join(dir, 'releases'), env = join(dir, 'launcher.env');
  const source = 'https://source.example', tail = 'https://node.fixture.ts.net', target = 'https://vps.example';
  const password = randomBytes(24).toString('base64url');
  const browserToken = 'bsi_' + randomBytes(32).toString('base64url');
  const digest = createHash('sha256').update(browserToken).digest('base64url');
  const publicKey = Buffer.concat([Buffer.from('0000000b7373682d6564323535313900000020', 'hex'), randomBytes(32)]).toString('base64');
  const declared = new Set<string>();
  const observations: string[] = [];
  const plans: Record<string, { tool: string; args: Record<string, unknown> }> = {
    machine: { tool: 'add_guide_machine', args: { name: 'Test machine', origin: target } },
    vps: { tool: 'install_guide_vps', args: { host: 'vps.example', user: 'install', origin: target, hostKey: 'ssh-ed25519 ' + publicKey, vaultEntry: 'fixture-vps' } },
    remote: { tool: 'configure_guide_remote_access', args: { origin: tail } },
    detect: { tool: 'inspect_guide_remote_access', args: {} },
    computer: { tool: 'setup_guide_computer', args: {} },
  };
  const provider = createServer(async (req, res) => {
    res.setHeader('Content-Type', 'application/json');
    if (req.method === 'GET') return res.end(JSON.stringify({ data: [{ id: 'fixture' }] }));
    let raw = '';
    for await (const chunk of req) raw += chunk;
    observations.push(raw);
    const input = JSON.parse(raw);
    for (const { function: fn } of input.tools ?? []) declared.add(fn.name);
    const prompt = input.messages.findLast((message: {role: string}) => message.role === 'user')?.content;
    const plan = plans[prompt];
    const tool = plan && input.messages.at(-1)?.role !== 'tool';
    res.end(JSON.stringify({ choices: [{ finish_reason: tool ? 'tool_calls' : 'stop', message: tool ? {
      content: null,
      tool_calls: [{ id: randomUUID(), type: 'function', function: { name: plan.tool, arguments: JSON.stringify({ ...plan.args, ...(prompt === 'detect' ? {} : { requestId: randomUUID() }) }) } }],
    } : { content: 'Review the setup card.' } }] }));
  });
  const previousEnv = process.env.MINARBOT_LAUNCHER_ENV_FILE;
  let app: Awaited<ReturnType<typeof startRustServer>> | undefined;
  try {
    await mkdir(bin);
    await mkdir(join(releases, '0.17.0-8523c26b'), { recursive: true });
    await symlink('0.17.0-8523c26b', join(releases, 'current'));
    await writeFile(env, 'MINARBOT_ALLOWED_ORIGINS=' + source + '\nUNRELATED="first\nsecond"\n', { mode: 0o600 });
    process.env.MINARBOT_LAUNCHER_ENV_FILE = env;
    const trace = join(dir, 'ssh-trace');
    const fixturePasswordHash = createHash('sha256').update(password).digest('hex');
    await writeFile(join(bin, 'ssh'), [
      '#!/bin/sh',
      'set -eu',
      'pass=$("$SSH_ASKPASS" "fixture password")',
      'hash=$(printf "%s" "$pass" | sha256sum)',
      '[ "$hash" = "' + fixturePasswordHash + '  -" ]',
      'printf "%s" "$pass"; printf "%s" "$pass" >&2',
      'for command do :; done',
      'case "$command" in',
      '  *rollback) printf "rollback\\n" >> "' + trace + '"; exit 0;;',
      'esac',
      'token=$(cat)',
      'case "$token" in bsi_*) ;; *) exit 2;; esac',
      'for argument do case "$argument" in *"$pass"*|*"$token"*) exit 3;; esac; done',
      'printf "%s" "$token"; printf "%s" "$token" >&2',
      'printf "install\\n" >> "' + trace + '"',
      'exit 1',
      '',
    ].join('\n'), { mode: 0o700 });
    await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
    const address = provider.address();
    assert(address && typeof address !== 'string');
    const database = join(dir, 'state.sqlite');
    app = await startRustServer({
      database, testKeyring: {}, testPathPrefix: bin, allowedOrigins: [source],
      baseURL: 'http://127.0.0.1:' + address.port + '/v1', apiKey: 'guide-provider-key', defaultModel: 'fixture',
      updates: { releasesDir: releases },
    });
    const local = app.origin;
    await writeFile(join(bin, 'tailscale'), [
      '#!/bin/sh',
      "case \"$*\" in",
      "  'status --json') printf '%s\\n' '" + JSON.stringify({
        BackendState: 'Running', CurrentTailnet: { MagicDNSEnabled: true },
        Self: { DNSName: 'node.fixture.ts.net.', UserID: 'fixture-private-peer-metadata' }, CertDomains: ['node.fixture.ts.net'],
      }) + "';;",
      "  'serve status --json') printf '%s\\n' '" + JSON.stringify({
        TCP: { 443: { HTTPS: true } }, Web: { 'node.fixture.ts.net:443': { Handlers: { '/': { Proxy: local } } } },
      }) + "';;",
      '  *) exit 9;;',
      'esac',
      '',
    ].join('\n'), { mode: 0o700 });
    async function api(path: string, body?: unknown, status = 200) {
      const res = await fetch(local + path, {
        method: body ? 'POST' : 'GET',
        headers: { Origin: local, 'Content-Type': 'application/json', 'Sec-Fetch-Site': 'same-origin' },
        body: body ? JSON.stringify(body) : undefined,
      });
      const raw = await res.text();
      observations.push(raw);
      assert.equal(res.status, status, path);
      return JSON.parse(raw);
    }
    const { guide } = await api('/api/agents');
    await api('/api/agents/' + guide.id + '/edit', {
      id: randomUUID(), revision: guide.revision, model: 'fixture', runtime: 'api', providerAccount: '', confirmRuntime: true,
    });
    const turns = '/api/agents/' + guide.id + '/turns';
    await api('/api/vault', { name: 'fixture-vps', kind: 'secret', revision: 0, url: target, value: password, agents: [guide.id] });
    async function send(text: string) {
      const id = randomUUID();
      await api(turns, { id, text }, 202);
      for (let n = 0; n < 100; n++) {
        const turn = (await api(turns)).find((turn: {id: string}) => turn.id === id);
        if (turn?.status === 'done') return turn.guide.changes[0];
        await setTimeout(70);
      }
      throw new Error('Guide fixture turn did not finish: ' + text);
    }
    async function apply(proposal: {id: string; change: unknown}, status = 200, action = 'apply') {
      return api('/api/guide/changes/' + proposal.id, { action, change: proposal.change }, status);
    }
    const machine = await send('machine');
    await api('/api/install/tokens', { id: machine.id, digest }, 409);
    await apply({ ...machine, change: { ...machine.change, target: { origin: source } } }, 409);
    assert.equal((await apply(machine)).installRequest, machine.id);
    const tokenReceipt = await api('/api/install/tokens', { id: machine.id, digest }, 201);
    assert.equal(tokenReceipt.source, source);
    assert.equal(tokenReceipt.token, undefined);
    await api('/api/install/tokens', { id: machine.id, digest }, 409);
    await apply(machine, 409);
    const invite = await fetch(local + '/api/install/pairing-code', {
      method: 'POST', headers: { Authorization: 'Bearer ' + browserToken },
    });
    assert.equal(invite.status, 200);
    const { code } = await invite.json();
    const denied = await fetch(local + '/api/pairing/redeem', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code, returnPairing: { id: randomUUID(), name: 'Wrong target', version: '0.17.0', origin: 'https://other.example' } }),
    });
    assert.equal(denied.status, 400, 'Pairing code must match the approved origin');

    const remote = await send('remote');
    assert(!String(await readFile(env)).includes(tail));
    await send('detect');
    assert(observations.some(value => value.includes('reachableFromServer') && value.includes('node.fixture.ts.net')));
    await apply(remote);
    const updated = await readFile(env, 'utf8');
    assert(updated.includes('MINARBOT_ALLOWED_ORIGINS=' + source + ',' + tail));
    assert(updated.includes('UNRELATED="first\nsecond"'));
    await api('/api/device-pairing/code', { origin: tail }, 403);
    const qr = await api('/api/device-pairing/code', { origin: source });
    assert.equal(qr.url, source + '/pair');
    assert(qr.qr.startsWith('data:image/svg+xml;base64,'));

    const vps = await send('vps');
    await assert.rejects(readFile(trace), { code: 'ENOENT' });
    await apply(vps, 200, 'cancel');
    await apply(vps, 409);
    await assert.rejects(readFile(trace), { code: 'ENOENT' });
    const failed = await apply(await send('vps'), 409);
    assert.match(failed.error, /stopped/);
    assert.equal(await readFile(trace, 'utf8'), 'install\nrollback\n');

    const computer = await apply(await send('computer'));
    assert.equal(typeof computer.setup.state, 'string');
    assert(declared.has('install_guide_vps') && declared.has('inspect_guide_remote_access') && !declared.has('run_shell'));
    observations.push(JSON.stringify(rustServerOutput(local)));
    for (const raw of observations) {
      assert(!raw.includes(password), 'SSH password leaked');
      assert(!raw.includes(browserToken), 'Install credential leaked');
      assert(!raw.includes(code), 'Machine pairing code leaked');
      assert(!raw.includes('fixture-private-peer-metadata'), 'Tailscale private status leaked');
      assert(!/bsi_[A-Za-z0-9_-]{43}/.test(raw), 'Server install credential leaked');
    }
    for (const file of await readdir(dir, { withFileTypes: true })) {
      if (!file.isFile() || !file.name.startsWith('state.sqlite')) continue;
      const bytes = await readFile(join(dir, file.name));
      assert(!bytes.includes(password) && !bytes.includes(browserToken), 'Credential persisted in plaintext');
      assert(!/bsi_[A-Za-z0-9_-]{43}/.test(bytes.toString('latin1')), 'Server install credential persisted in plaintext');
    }
  } finally {
    if (previousEnv === undefined) delete process.env.MINARBOT_LAUNCHER_ENV_FILE;
    else process.env.MINARBOT_LAUNCHER_ENV_FILE = previousEnv;
    await app?.close();
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    await rm(dir, { recursive: true });
  }
});
