import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync, chmodSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer, request as httpRequest, type Server } from 'node:http';
import { setTimeout as delay } from 'node:timers/promises';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';
import { fakePeerTokenIssuer } from './test-support/peer-token-issuer.ts';

const appVersion = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')).version;
type App = Awaited<ReturnType<typeof startApp>>;
async function call(app: { origin: string }, path: string, body?: unknown, status = 200, bearer?: string, browser = true): Promise<any> {
  const response = await fetch(app.origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { ...(browser ? { Origin: app.origin } : {}), 'Content-Type': 'application/json', Connection: 'close', ...(bearer ? { Authorization: `Bearer ${bearer}` } : {}) }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  const result = await response.json(); assert.equal(response.status, status, JSON.stringify(result)); return result;
}
const command = () => ({ id: randomUUID() });
const environment = (app: App) => call(app, '/api/environment');
const list = async (app: App) => (await call(app, '/api/machines')).machines;
const pair = async (a: App, b: App) => { const code = await call(b, '/api/pairing-codes', command()); const request = { ...command(), origin: b.origin, code: code.code }; return { result: await call(a, '/api/machines', request), code, request }; };
async function listen(server: Server) { await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve)); const address = server.address(); assert(address && typeof address !== 'string'); return { origin: `http://127.0.0.1:${address.port}` }; }
async function close(server: Server) { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); }

test('paired machine recovers after restart while roster sync times out on a shared link', { timeout: 60_000 }, async t => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-peer-throttle-'));
  const a = await startApp({ database: join(dir, 'a.sqlite'), machines: { healthIntervalMs: 1000 } });
  t.after(() => a.close());
  const options = { database: join(dir, 'b.sqlite') };
  let b = await startApp(options);
  t.after(() => b.close());
  let route = '', slow = false, probeTimedOut = false, rosterBytes = 0;
  const proxy = createServer((req, res) => {
    if (probeTimedOut && req.url === '/api/environment') {
      res.writeHead(504).end();
      return;
    }
    const upstream = httpRequest(b.origin + req.url, { method: req.method, headers: { ...req.headers, host: new URL(b.origin).host } }, async source => {
      const chunks: Buffer[] = [];
      try {
        for await (const chunk of source) chunks.push(chunk);
      } catch {
        res.destroy();
        return;
      }
      let body = Buffer.concat(chunks);
      if (req.url === '/api/environment' || req.url === '/api/pairing/redeem') {
        const value = JSON.parse(body.toString());
        (value.environment ?? value).origin = route;
        body = Buffer.from(JSON.stringify(value));
      }
      if (slow) await delay(120);
      if (res.destroyed) return;
      if (slow && req.url === '/api/peer/agents' && body.length > 100 && source.statusCode === 200) {
        body = Buffer.concat([body, Buffer.from(' '.repeat(120 * 1024))]);
        rosterBytes = body.length;
        res.writeHead(source.statusCode ?? 502, { 'Content-Type': 'application/json', 'Content-Length': body.length });
        // Desktop traffic uses 15 KiB/s of the 25 KiB/s uplink.
        for (let offset = 0; offset < body.length && !res.destroyed; offset += 1024) {
          await delay(100);
          if (!res.destroyed) res.write(body.subarray(offset, offset + 1024));
        }
        res.end();
      } else {
        res.writeHead(source.statusCode ?? 502, { 'Content-Type': 'application/json', 'Content-Length': body.length });
        res.end(body);
      }
    });
    upstream.once('error', () => res.destroy());
    req.pipe(upstream);
  });
  route = (await listen(proxy)).origin;
  t.after(() => close(proxy));
  t.after(() => rmSync(dir, { recursive: true, force: true }));

  await call(b, '/api/agents', {
    id: randomUUID(), runtime: 'api', name: 'Remote agent', role: 'Personal agent',
    model: 'fixture', instructions: 'Help.',
  }, 201);
  const code = await call(b, '/api/pairing-codes', command());
  const paired = await call(a, '/api/machines', { ...command(), origin: route, code: code.code });
  assert.equal(paired.agentCount, 1);
  assert.equal(paired.status, 'online');
  await b.close();
  await a.refreshMachines();
  assert.equal((await list(a))[0].status, 'offline');

  slow = true;
  b = await startApp(options);
  const restarted = performance.now();
  const deadline = restarted + 8_000;
  let peer = (await list(a))[0];
  while ((peer.status !== 'online' || !rosterBytes) && performance.now() < deadline) {
    await delay(50);
    peer = (await list(a))[0];
  }
  t.diagnostic(`Peer status after ${Math.round(performance.now() - restarted)} ms at 10 KiB/s plus 120 ms latency: ${peer.status}, roster ${rosterBytes} bytes`);
  assert(rosterBytes > 100 * 1024, 'Roster takes over 10 seconds on the shared link');
  assert.equal(peer.status, 'online');
  assert(peer.lastSeen > paired.lastSeen);
  assert.equal(peer.agentCount, paired.agentCount);

  await a.refreshMachines();
  peer = (await list(a))[0];
  assert.equal(peer.status, 'online', 'Roster deadline leaves authenticated liveness intact');
  assert.deepEqual(peer.agents, paired.agents);

  probeTimedOut = true;
  await a.refreshMachines();
  const stale = (await list(a))[0];
  assert.equal(stale.status, 'stale');
  assert(stale.lastSeen >= peer.lastSeen);
  assert.deepEqual(stale.agents, paired.agents);
});

test('peer transport accepts proxy host headers while owner machine routes retain the local host boundary', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-peer-host-'));
  const app = await startApp({ database: join(dir, 'db.sqlite'), peerTokenIssuer: fakePeerTokenIssuer() });
  const send = (path: string, body?: unknown) => new Promise<number>((resolve, reject) => {
    const req = httpRequest(app.origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { Host: 'workstation.example.test:4317', 'Content-Type': 'application/json' } }, res => { res.resume(); res.once('end', () => resolve(res.statusCode!)); });
    req.once('error', reject); req.end(body === undefined ? undefined : JSON.stringify(body));
  });
  try {
    assert.equal(await send('/api/environment'), 200);
    assert.equal(await send('/api/peer/agents'), 401);
    assert.equal((await fetch(app.origin + '/api/peer/agents', { headers: { 'X-Minarbot-Peer-Probe': '1' } })).status, 401);
    assert.equal(await send('/api/pairing/redeem', {}), 400);
    assert.equal(await send('/api/machines/reciprocal', {}), 401);
    assert.equal(await send('/api/peer/unpair', {}), 401);
    assert.equal(await send('/api/machines'), 403);
    assert.equal(await send('/api/pairing-codes', command()), 403);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});

test('two real instances pair mutually, expose scoped cached agents, survive restarts, recover offline, and revoke on unpair without leaking credentials', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-machines-')), issuerA = fakePeerTokenIssuer(), issuerB = fakePeerTokenIssuer();
  const aOptions = { database: join(dir, 'a.sqlite'), peerTokenIssuer: issuerA }, bOptions = { database: join(dir, 'b.sqlite'), peerTokenIssuer: issuerB };
  let a = await startApp(aOptions), b = await startApp(bOptions); const logs: string[] = [];
  const original = [console.log, console.warn, console.error]; console.log = console.warn = console.error = (...args: unknown[]) => { logs.push(args.map(String).join(' ')); };
  try {
    const initialA = await environment(a), initialB = await environment(b);
    assert.notEqual(initialA.id, initialB.id);
    const rename = { ...command(), name: 'Machine B' }; await call(b, '/api/environment', rename); await call(b, '/api/environment', rename); await call(b, '/api/environment', { ...rename, name: 'Other' }, 409);
    const agentA = { runtime: 'api', id: randomUUID(), name: 'Alpha', role: 'Personal agent', model: 'fixture', instructions: 'Help.' }, agentB = { ...agentA, id: randomUUID(), name: 'Beta' };
    await call(a, '/api/agents', agentA, 201); await call(b, '/api/agents', agentB, 201);
    const { result, code, request } = await pair(a, b); assert.equal(result.id, initialB.id); assert.equal(result.pairingState, 'paired'); assert.equal(result.status, 'online'); assert.equal(result.agentCount, 1); assert.equal(result.agents[0].name, 'Beta'); assert.equal(result.agents[0].role, 'Personal agent');
    await call(a, '/api/machines', request); assert.equal(issuerA.issued.length, 1); assert.equal(issuerB.issued.length, 1);
    const back = (await list(b))[0]; assert.equal(back.id, initialA.id); assert.equal(back.agents[0].name, 'Alpha');
    assert.deepEqual((await call(b, '/api/peer/agents', undefined, 200, issuerB.issued[0], false)).agents, result.agents);
    await call(b, '/api/peer/agents', undefined, 401, undefined, false); await call(b, '/api/peer/agents', undefined, 401, 'wrong-peer-token', false);
    const auth = issuerB.tokens.get(issuerB.issued[0])!; issuerB.tokens.set(issuerB.issued[0], { ...auth, scopes: ['history:read'] });
    await call(b, '/api/peer/agents', undefined, 401, issuerB.issued[0], false); await a.refreshMachines(); assert.equal((await list(a))[0].status, 'unauthorized'); issuerB.tokens.set(issuerB.issued[0], auth);
    await call(b, '/api/pairing/redeem', { code: code.code, returnPairing: initialA }, 400, undefined, false);
    const bPort = Number(new URL(b.origin).port), aPort = Number(new URL(a.origin).port);
    await b.close(); await a.refreshMachines(); assert.equal((await list(a))[0].status, 'offline'); assert.equal((await list(a))[0].agents[0].name, 'Beta');
    b = await startApp({ ...bOptions, port: bPort }); assert.equal((await environment(b)).id, initialB.id); assert.equal((await environment(b)).name, 'Machine B'); await a.refreshMachines(); assert.equal((await list(a))[0].status, 'online');
    await a.close(); a = await startApp({ ...aOptions, port: aPort }); assert.equal((await environment(a)).id, initialA.id); await b.refreshMachines(); assert.equal((await list(b))[0].status, 'online'); await a.refreshMachines();
    const codeRequest = command(), once = await call(a, '/api/pairing-codes', codeRequest); await call(a, '/api/pairing-codes', codeRequest, 409);
    for (const app of [a, b]) for (const path of ['/api/environment', '/api/machines', '/api/agents']) { const response = JSON.stringify(await call(app, path)); for (const token of [...issuerA.issued, ...issuerB.issued]) assert(!response.includes(token)); }
    await a.close(); await b.close();
    for (const file of readdirSync(dir, { recursive: true, withFileTypes: true })) {
      if (!file.isFile() || file.name.endsWith('-key')) continue;
      const path = join(file.parentPath, file.name), bytes = readFileSync(path);
      for (const token of [...issuerA.issued, ...issuerB.issued, code.code, once.code]) assert(!bytes.includes(Buffer.from(token)), `Secret in ${path}`);
    }
    for (const file of ['a.sqlite.peer-key', 'b.sqlite.peer-key']) assert.equal(statSync(join(dir, file)).mode & 0o777, 0o600);
    assert(!logs.join('\n').includes('peer-canary-'));
    a = await startApp({ ...aOptions, port: aPort }); b = await startApp({ ...bOptions, port: bPort });
    const removal = { ...command(), environmentId: initialB.id }; await call(a, '/api/machines/remove', removal); await call(a, '/api/machines/remove', removal); assert.deepEqual(await list(a), []); assert.deepEqual(await list(b), []); assert.equal(issuerA.tokens.size, 0); assert.equal(issuerB.tokens.size, 0);
    await call(b, '/api/peer/agents', undefined, 401, issuerB.issued[0], false);
  } finally { [console.log, console.warn, console.error] = original; await a.close().catch(() => {}); await b.close().catch(() => {}); rmSync(dir, { recursive: true, force: true }); }
});

test('pairing uses uniform invalid-code errors, strict bodies, one-time expiry, persisted rate limits, and browser-only owner writes', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-pairing-')); let clock = Date.now(); const issuer = fakePeerTokenIssuer(), options = { database: join(dir, 'db.sqlite'), peerTokenIssuer: issuer, machines: { now: () => clock } }; let app = await startApp(options);
  try {
    const remote = { id: randomUUID(), name: 'Remote', version: appVersion, origin: 'http://127.0.0.1:12345' };
    const redeem = (body: unknown, status = 400) => call(app, '/api/pairing/redeem', body, status, undefined, false);
    const made = await call(app, '/api/pairing-codes', command()); clock += 10 * 60_000 + 1;
    const wrong = await redeem({ code: '23456789ABCD', returnPairing: remote }); assert.deepEqual(await redeem({ code: made.code, returnPairing: remote }), wrong);
    for (const body of [null, [], {}, { code: 42 }, { code: '23456789ABCD', returnPairing: { ...remote, extra: true } }, { code: '23456789ABCD', returnPairing: remote, extra: true }]) assert.deepEqual(await redeem(body), wrong);
    for (let i = 8; i < 12; i++) await redeem({ code: '23456789ABCD', returnPairing: remote }); await redeem({}, 429);
    const port = Number(new URL(app.origin).port); await app.close(); app = await startApp({ ...options, port }); await redeem({}, 429); clock += 120_000;
    const valid = await call(app, '/api/pairing-codes', command()); const response = await redeem({ code: valid.code.toLowerCase(), returnPairing: remote }, 200); assert(response.token); await redeem({ code: valid.code, returnPairing: remote });
    await call(app, '/api/machines/reciprocal', { environment: { ...remote, id: randomUUID() }, token: 'return-canary-token-123456' }, 401, response.token, false);
    await call(app, '/api/pairing-codes', command(), 403, undefined, false);
    await call(app, '/api/pairing/redeem', { code: valid.code, returnPairing: remote }, 400);
    await call(app, '/api/peer/unpair', {}, 401, 'invalid-token-123456', false);
    await call(app, '/api/environment', { ...command(), name: '', extra: 'x' }, 400);
    await call(app, '/api/environment?x=1', undefined, 400);
    clock += 10 * 60_000 + 1; await app.refreshMachines(); assert.equal(issuer.tokens.size, 0);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});

test('origins reject unsafe forms; remote pairing rejects redirects, oversized bodies, and credential reflection', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-pair-fetch-')); const issuer = fakePeerTokenIssuer(), app = await startApp({ database: join(dir, 'db.sqlite'), peerTokenIssuer: issuer }); let mode = 'redirect', targetCalls = 0;
  const destination = createServer((_req, res) => { targetCalls++; res.end('{}'); }), destinationAddress = await listen(destination);
  const fake = createServer((_req, res) => { res.setHeader('Content-Type', 'application/json'); if (mode === 'redirect') { res.writeHead(307, { Location: destinationAddress.origin + '/steal' }); return res.end(); } if (mode === 'large') return res.end(JSON.stringify({ value: 'x'.repeat(129 * 1024) })); if (mode === 'declared') { res.writeHead(200, { 'Content-Length': '9999999' }); return res.end('{}'); } res.end('{}'); }); const address = await listen(fake);
  try {
    for (mode of ['redirect', 'large', 'declared', 'invalid']) await call(app, '/api/machines', { ...command(), origin: address.origin, code: '23456789ABCD' }, mode === 'invalid' ? 400 : 502);
    assert.equal(targetCalls, 0); assert.deepEqual(await list(app), []); assert.equal(issuer.tokens.size, 0);
  } finally { await app.close(); await close(fake); await close(destination); rmSync(dir, { recursive: true, force: true }); }
});

test('unpair removes first while remote is offline; missing wrapping key fails closed and cached agents remain readable', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-peer-recovery-')), issuerA = fakePeerTokenIssuer(), issuerB = fakePeerTokenIssuer(); const aOptions = { database: join(dir, 'a.sqlite'), peerTokenIssuer: issuerA }, bOptions = { database: join(dir, 'b.sqlite'), peerTokenIssuer: issuerB };
  const a = await startApp(aOptions), b = await startApp(bOptions);
  try {
    await pair(a, b); chmodSync(aOptions.database + '.peer-key', 0o644); await a.refreshMachines(); assert.equal((await list(a))[0].status, 'unauthorized'); chmodSync(aOptions.database + '.peer-key', 0o600);
    await b.close(); const target = (await list(a))[0]; await call(a, '/api/machines/remove', { ...command(), environmentId: target.id }); assert.deepEqual(await list(a), []); assert.equal(issuerA.tokens.size, 0);
  } finally { await a.close(); await b.close().catch(() => {}); rmSync(dir, { recursive: true, force: true }); }
});

test('background health records a different peer version as online and never sends bearer to a changed identity or redirected descriptor', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-peer-health-')), issuer = fakePeerTokenIssuer(); const app = await startApp({ database: join(dir, 'db.sqlite'), peerTokenIssuer: issuer, machines: { healthIntervalMs: 50 } });
  const version = (await environment(app)).version, id = randomUUID(), remoteToken = 'remote-peer-canary-1234567890123456', agent = { id: randomUUID(), name: 'Remote agent', role: 'Personal agent', model: 'fixture', status: 'idle' }; let mode = 'normal', origin = '', authorizedCalls = 0, issuedBack = '';
  const server = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const data = raw ? JSON.parse(raw) : {};
    res.setHeader('Content-Type', 'application/json'); const send = (body: unknown) => res.end(JSON.stringify(body));
    const descriptor = { id: mode === 'identity' ? randomUUID() : id, name: 'Synthetic workstation', origin, version: mode === 'version' ? '99.0.0' : version };
    if (req.headers.authorization) authorizedCalls++;
    if (req.url === '/api/pairing/redeem') return send({ environment: descriptor, token: remoteToken });
    if (req.url === '/api/machines/reciprocal') { issuedBack = data.token; return send({ ok: true }); }
    if (req.url === '/api/environment') { if (mode === 'redirect') { res.writeHead(302, { Location: app.origin + '/api/environment' }); return res.end(); } return send(descriptor); }
    if (req.url === '/api/peer/agents') { if (mode === 'unauthorized') { res.writeHead(401); return send({ error: 'no' }); } return send({ agents: [{ ...agent, name: mode === 'reflection' ? issuedBack : agent.name }], ...(mode === 'bad-policy' ? { messagingAllowed: 'false' } : {}) }); }
    return send({ ok: true });
  }); origin = (await listen(server)).origin;
  // Finish any background probe before changing this fixture's remote state.
  async function setMode(next: string) { await app.refreshMachines(); mode = next; await app.refreshMachines(); }
  try {
    const result = await call(app, '/api/machines', { ...command(), origin, code: '23456789ABCD' }); assert.equal(result.status, 'online');
    mode = 'version'; const deadline = Date.now() + 20_000;
    // A different app version is recorded but stays online: rolling updates must not cut paired machines off.
    while ((await list(app))[0].version !== '99.0.0' && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 50));
    assert.equal((await list(app))[0].version, '99.0.0');
    assert.equal((await list(app))[0].status, 'online');
    await setMode('normal'); assert.equal((await list(app))[0].status, 'online'); assert.equal((await list(app))[0].remoteAgentMessaging, undefined, 'Older peers omit availability hints');
    await setMode('bad-policy'); assert.equal((await list(app))[0].status, 'offline');
    await setMode('unauthorized'); assert.equal((await list(app))[0].status, 'unauthorized');
    await setMode('identity'); const before = authorizedCalls; await app.refreshMachines(); assert.equal((await list(app))[0].status, 'unauthorized'); assert.equal(authorizedCalls, before, 'Identity mismatch received bearer');
    await setMode('redirect'); assert.equal((await list(app))[0].status, 'offline'); assert.equal(authorizedCalls, before);
    await setMode('reflection'); assert.equal((await list(app))[0].status, 'offline'); assert.equal((await list(app))[0].agents[0].name, agent.name); assert(!JSON.stringify(await list(app)).includes(issuedBack));
  } finally { await app.close(); await close(server); rmSync(dir, { recursive: true, force: true }); }
});

test('lost reciprocal acknowledgement retries encrypted pending credentials after restart without minting another token', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-peer-retry-')), issuer = fakePeerTokenIssuer(), options = { database: join(dir, 'db.sqlite'), peerTokenIssuer: issuer };
  let app = await startApp(options), origin = '', attempts = 0, saved = '';
  const peerId = randomUUID(), token = 'retry-peer-canary-12345678901234567890';
  const server = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk; const data = raw ? JSON.parse(raw) : {};
    res.setHeader('Content-Type', 'application/json'); const send = (body: unknown) => res.end(JSON.stringify(body));
    const environment = { id: peerId, name: 'Retry workstation', version: appVersion, origin };
    if (req.url === '/api/pairing/redeem') return send({ environment, token });
    if (req.url === '/api/machines/reciprocal') { attempts++; if (!saved) { saved = data.token; return req.socket.destroy(); } assert.equal(data.token, saved); return send({ ok: true }); }
    if (req.url === '/api/environment') return send(environment);
    if (req.url === '/api/peer/agents') return send({ agents: [] }); return send({ ok: true });
  }); origin = (await listen(server)).origin;
  try {
    const request = { ...command(), origin, code: '23456789ABCD' }; await call(app, '/api/machines', request, 409); assert.equal((await list(app))[0].pairingState, 'pending');
    const port = Number(new URL(app.origin).port); await app.close(); app = await startApp({ ...options, port });
    assert.equal((await call(app, '/api/machines', request)).pairingState, 'paired'); assert.equal(attempts, 2); assert.equal(issuer.issued.length, 1);
    const mismatch = { ...request, code: '23456789ABCE' }; await call(app, '/api/machines', mismatch, 409);
    await app.close(); const db = testDatabase(options.database); try { const row = db.prepare('SELECT token,return_token FROM paired_machines').get()!; assert(!String(row.token).includes(token)); assert(!String(row.return_token).includes(saved)); } finally { db.close(); }
  } finally { await app.close().catch(() => {}); await close(server); rmSync(dir, { recursive: true, force: true }); }
});

test('default wiring issues pairing tokens from the Minarbot MCP token registry and revokes them on unpair', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-machines-mcp-'));
  const a = await startApp({ database: join(dir, 'a.sqlite') }), b = await startApp({ database: join(dir, 'b.sqlite') });
  try {
    await call(b, '/api/environment', { ...command(), name: 'Machine B' }); await call(a, '/api/environment', { ...command(), name: 'Machine A' });
    await call(b, '/api/agents', { runtime: 'api', id: randomUUID(), name: 'Beta', role: 'Personal agent', model: 'fixture', instructions: 'Help.' }, 201);
    const { result } = await pair(a, b); assert.equal(result.pairingState, 'paired'); assert.equal(result.status, 'online'); assert.equal(result.agents[0].name, 'Beta'); assert.equal(result.agents[0].role, 'Personal agent');
    const tokensOnB = (await call(b, '/api/mcp-server/peer-tokens')).items; assert.equal(tokensOnB.length, 1); assert.equal(tokensOnB[0].name, 'Minarbot: Machine A'); assert.equal(tokensOnB[0].revoked, null);
    assert.deepEqual([...tokensOnB[0].scopes].sort(), ['agents:message', 'agents:read', 'history:read']);
    assert.equal((await call(a, '/api/mcp-server/peer-tokens')).items[0].name, 'Minarbot: Machine B');
    await call(a, '/api/machines/remove', { ...command(), environmentId: result.id });
    assert.deepEqual(await list(a), []); assert.deepEqual(await list(b), []);
    assert((await call(b, '/api/mcp-server/peer-tokens')).items[0].revoked, 'B revoked the token it issued to A');
  } finally { await a.close().catch(() => {}); await b.close().catch(() => {}); rmSync(dir, { recursive: true, force: true }); }
});
