import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { request as httpRequest, type IncomingMessage } from 'node:http';
import { connect } from 'node:net';
import { once } from 'node:events';
import { testDatabase } from '../test-support/database.ts';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { hubCommand, startRustHub, type HostedHubConfig, type SessionIdentity } from '../test-support/hosted-hub.ts';
import { rustBinary } from '../rust-binary.ts';

const accounts = 'accounts.minarbot.test', issuer = 'https://clerk.example.test';
const origin = (host: string) => `https://${host}`;
type Response = { status: number; headers: IncomingMessage['headers']; data: any; raw: string; cookies: string[] };
type Project = { host: string; projectId: string; sectionId: string };
type Settings = { tokens?: Record<string, SessionIdentity>; clerk?: boolean; trusted?: string[]; nodeLink?: boolean; omitInvite?: boolean };
function files(dir: string, tokens: Record<string, SessionIdentity>, settings: Settings) {
  const dataPath = resolve(dir, 'hub.sqlite');
  const mailboxKeyPath = resolve(dir, 'mailbox.key');
  const tokensPath = resolve(dir, 'tokens.json');
  const jwksPath = resolve(dir, 'jwks.json');
  const signingKeyPath = resolve(dir, 'signer.pem');
  writeFileSync(mailboxKeyPath, Buffer.alloc(32, 8), { mode: 0o600 });
  writeFileSync(tokensPath, JSON.stringify(tokens), { mode: 0o600 });
  writeFileSync(jwksPath, JSON.stringify({ keys: [jwk] }), { mode: 0o600 });
  if (settings.nodeLink) {
    const signer = generateKeyPairSync('ed25519').privateKey.export({ type: 'pkcs8', format: 'pem' });
    writeFileSync(signingKeyPath, signer, { mode: 0o600 });
  }
  const config: HostedHubConfig = {
    domain: 'minarbot.test', accountsHost: accounts, dataPath, mailboxKeyPath, trustedGatewayAddresses: settings.trusted,
    identity: settings.clerk ? { mode: 'clerk', issuer, audience: 'minarbot-hub', jwksPath } : { mode: 'stub', tokensPath },
    nodeLink: settings.nodeLink ? { signingKeyPath } : undefined,
  };
  return { config, jwksPath };
}

function serverBinary() {
  return rustBinary('minarbot-server', process.env.MINARBOT_HOSTED_HUB_BINARY ?? process.env.MINARBOT_RUST_BINARY);
}

async function fixture(t: TestContext, settings: Settings = {}) {
  const dir = mkdtempSync(resolve(tmpdir(), 'minarbot-hub-gateway-'));
  const binary = serverBinary();
  let clock = Date.now();
  const subjects = ['alice', 'bob', 'eve', 'outside'] as const;
  const identities = Object.fromEntries(subjects.map(subject => [subject, {
    issuer, subject, sessionId: subject + '-sid', expiresAt: clock + 3600000,
    verifiedMailboxes: [subject + '@example.test'],
  } satisfies SessionIdentity]));
  const { config } = files(dir, { ...identities, ...settings.tokens }, settings);
  const fixedClock = !settings.clerk && !settings.trusted?.length;
  let hub: Awaited<ReturnType<typeof startRustHub>> | undefined;
  let db: ReturnType<typeof testDatabase> | undefined;
  t.after(async () => { await hub?.close(); db?.close(); rmSync(dir, { recursive: true }); });
  const ids = Object.fromEntries(subjects.filter(subject => subject !== 'outside').map(subject => [subject,
    String(JSON.parse(hubCommand(binary, dir, config, 'admit', issuer, subject)).accountId)]));
  async function reopen() {
    await hub?.close();
    hub = await startRustHub(binary, dir, config, 0, fixedClock);
    if (fixedClock) await hub.control({ now: clock });
  }
  await reopen();
  const database = testDatabase(config.dataPath);
  db = database;
  const port = () => hub!.port;
  const one = (sql: string) => database.prepare(sql).get();
  function request(host: string, path: string, options: { method?: string; headers?: Record<string, string>; body?: unknown; rawBody?: string; cookies?: string[]; csrf?: string; origin?: string | null } = {}): Promise<Response> {
    return new Promise((resolve, reject) => {
      const method = options.method ?? (options.body !== undefined || options.rawBody !== undefined ? 'POST' : 'GET');
      const headers: Record<string, string> = { Host: host, ...options.headers };
      if (options.cookies) headers.Cookie = options.cookies.join('; ');
      if (options.csrf) headers['X-CSRF-Token'] = options.csrf;
      if (options.origin !== null && (method !== 'GET' || options.origin)) headers.Origin = options.origin ?? origin(host);
      if (options.body !== undefined || options.rawBody !== undefined) headers['Content-Type'] ??= 'application/json';
      const req = httpRequest({ host: '127.0.0.1', port: port(), path, method, headers }, res => {
        let raw = ''; res.setEncoding('utf8'); res.on('data', chunk => raw += chunk); res.on('end', () => {
          let data: unknown; try { data = JSON.parse(raw); } catch { data = undefined; }
          resolve({ status: res.statusCode!, headers: res.headers, data, raw, cookies: (res.headers['set-cookie'] ?? []).map(cookie => cookie.split(';')[0]) });
        });
      });
      req.setTimeout(5000, () => req.destroy(new Error('Hosted request timed out')));
      req.on('error', reject); req.end(options.rawBody ?? (options.body !== undefined ? JSON.stringify(options.body) : undefined));
    });
  }
  async function authenticate(token: string) {
    const bootstrap = await request(accounts, '/auth/bootstrap'); assert.equal(bootstrap.status, 200);
    return request(accounts, '/auth/session', { body: {}, cookies: bootstrap.cookies, csrf: bootstrap.data.csrf, headers: { Authorization: 'Bearer ' + token } });
  }
  async function central(subject = 'alice') {
    const token = settings.clerk && !subject.includes('.') ? jwt(claims(subject)) : subject;
    const result = await authenticate(token);
    assert.equal(result.status, 200, result.raw);
    return result;
  }
  async function project(subject: string, slug: string, name: string): Promise<Project> {
    const session = await central(subject);
    const result = await request(accounts, '/api/projects', { body: { slug, name }, cookies: session.cookies, csrf: session.data.csrf });
    assert.equal(result.status, 200, result.raw);
    return result.data;
  }
  const alpha = await project('alice', 'alpha', 'Alpha'), beta = await project('bob', 'beta', 'Beta');
  async function start(host = alpha.host) { const result = await request(host, '/auth/start'); assert.equal(result.status, 200); return result; }
  async function handoff(login: Response, started: Response) {
    const result = await request(accounts, '/auth/handoff', { body: { state: started.data.state }, cookies: login.cookies, csrf: login.data.csrf });
    assert.equal(result.status, 200, result.raw); const url = new URL(result.data.callback), fragment = new URLSearchParams(url.hash.slice(1));
    return { state: fragment.get('state'), code: fragment.get('code') };
  }
  async function redeem(host: string, started: Response, code: unknown, cookies = started.cookies) {
    return request(host, '/auth/redeem', { body: code, cookies, csrf: started.data.state });
  }
  async function login(subject = 'alice', host = alpha.host, parent?: Response) {
    const centralSession = parent ?? await central(subject), started = await start(host), code = await handoff(centralSession, started);
    const result = await redeem(host, started, code); assert.equal(result.status, 200, result.raw); return result;
  }
  async function stream(session: Response, host = alpha.host, sectionId = alpha.sectionId) {
    const req = httpRequest({ host: '127.0.0.1', port: port(), path: `/api/events?sectionId=${sectionId}`, headers: { Host: host, Cookie: session.cookies.join('; ') } });
    const response = new Promise<IncomingMessage>((resolve, reject) => { req.on('response', resolve); req.on('error', reject); });
    req.end(); const res = await response; assert.equal(res.statusCode, 200); res.resume();
    const closed = new Promise<void>((resolve, reject) => { const timer = setTimeout(() => reject(new Error('SSE did not close')), 2000); res.on('end', () => { clearTimeout(timer); resolve(); }); res.on('close', () => { clearTimeout(timer); resolve(); }); });
    t.after(() => req.destroy()); return { res, closed };
  }
  async function raw(headers: string, path = '/api/project') {
    const socket = connect(port(), '127.0.0.1'); await once(socket, 'connect'); let result = '';
    socket.setEncoding('utf8'); socket.on('data', chunk => result += chunk); socket.setTimeout(5000, () => socket.destroy(new Error('Raw request timed out'))); socket.write(`GET ${path} HTTP/1.1\r\n${headers}\r\nConnection: close\r\n\r\n`); await once(socket, 'close'); return result;
  }
  const alice = await login();
  if (!settings.omitInvite) {
    const bob = await login('bob');
    const invitation = await request(alpha.host, '/api/invitations', { body: { mailbox: 'bob@example.test', role: 'member' }, cookies: alice.cookies, csrf: alice.data.csrf });
    assert.equal(invitation.status, 200, invitation.raw);
    const proof = Object.fromEntries(new URLSearchParams(new URL(invitation.data.url).hash.slice(1)));
    const accepted = await request(alpha.host, '/api/invitations/accept', { body: proof, cookies: bob.cookies, csrf: bob.data.csrf });
    assert.equal(accepted.status, 200, accepted.raw);
  }
  async function editMember(accountId: string, revision: number, role: string) {
    const result = await request(alpha.host, '/api/members', { body: { accountId, revision, role }, cookies: alice.cookies, csrf: alice.data.csrf });
    assert.equal(result.status, 200, result.raw);
  }
  return {
    db: database, ids, identities, alpha, beta, request, central, start, handoff, redeem, login, stream, raw, authenticate, port, reopen, one, editMember,
    async advance(ms: number) { await hub!.control({ now: clock + ms }); clock += ms; },
  };
}

test('gateway: raw Host allowlist, reserved names, duplicates, aliases, uniform missing and forwarded-host denial', async t => {
  const f = await fixture(t);
  const unknown = await f.request('missing.minarbot.test', '/api/project'); assert.equal(unknown.status, 404);
  for (const host of ['minarbot.test', 'www.minarbot.test', 'api.minarbot.test', 'ALPHA.minarbot.test', 'alpha.minarbot.test.', 'alpha.minarbot.test:443', 'alpha.minarbot.test.evil.test', 'xn--abc.minarbot.test', 'hub-origin.minarbot.test']) {
    const result = await f.request(host, '/api/project'); assert.equal(result.status, 404, host); assert.deepEqual(result.data, unknown.data);
  }
  for (const headers of [{ 'X-Forwarded-Host': f.beta.host }, { Forwarded: `host=${f.beta.host}` }, { 'X-Forwarded-Host': `${f.alpha.host},${f.beta.host}` }] as Record<string, string>[]) assert.equal((await f.request(f.alpha.host, '/auth/start', { headers })).status, 404);
  assert.match(await f.raw(`Host: ${f.alpha.host}\r\nHost: ${f.beta.host}`), /^HTTP\/1.1 404/);
  assert.match(await f.raw(''), /^HTTP\/1.1 400/);
  const scoped = await f.login('alice', f.alpha.host);
  assert.equal((await f.request(f.beta.host, '/api/project', { cookies: scoped.cookies })).status, 401);
});

test('gateway: forwarded Host works only for configured exact gateway address; no forwarded chain', async t => {
  const f = await fixture(t, { trusted: ['127.0.0.1'] });
  assert.equal((await f.request('internal.example.test', '/auth/start', { headers: { 'X-Forwarded-Host': f.alpha.host } })).status, 200);
  assert.equal((await f.request('internal.example.test', '/auth/start', { headers: { 'X-Forwarded-Host': `${f.alpha.host}, ${f.beta.host}` } })).status, 404);
  assert.equal((await f.request('internal.example.test', '/auth/start', { headers: { Forwarded: `host=${f.alpha.host}` } })).status, 404);
});

test('handoff: browser-bound, wrong host does not consume, single use with one concurrent winner', async t => {
  const f = await fixture(t), parent = await f.central(), started = await f.start(), code = await f.handoff(parent, started);
  const foreignBrowser = await f.start();
  assert.equal((await f.redeem(f.alpha.host, started, code, foreignBrowser.cookies)).status, 401);
  assert.equal((await f.redeem(f.beta.host, started, code)).status, 401);
  const results = await Promise.all([f.redeem(f.alpha.host, started, code), f.redeem(f.alpha.host, started, code)]);
  assert.deepEqual(results.map(result => result.status).sort(), [200, 401]);
  const session = results.find(result => result.status === 200)!;
  for (const header of session.headers['set-cookie']!) {
    assert.match(header, /^__Host-/); assert.match(header, /; Secure; HttpOnly; SameSite=Lax; Max-Age=/); assert(!header.includes('Domain='));
  }
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: session.cookies })).status, 200);
  assert.equal((await f.request(f.beta.host, '/api/session', { cookies: session.cookies })).status, 401);
  assert.equal((await f.request(accounts, '/api/session', { cookies: session.cookies })).status, 401);
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: parent.cookies })).status, 401);
});

test('handoff: 60-second code, five-minute state, replacement and token expiry bound every session', async t => {
  const f = await fixture(t), parent = await f.central(), started = await f.start(), first = await f.handoff(parent, started), second = await f.handoff(parent, started);
  assert.equal((await f.redeem(f.alpha.host, started, first)).status, 401);
  await f.advance(60000); assert.equal((await f.redeem(f.alpha.host, started, second)).status, 401);
  await f.advance(240000);
  assert.equal((await f.request(accounts, '/auth/handoff', { body: { state: started.data.state }, cookies: parent.cookies, csrf: parent.data.csrf })).status, 401);
  const shortIdentity = { ...f.identities.alice, expiresAt: Date.now() + 20000 };
  const short = await fixture(t, { tokens: { alice: shortIdentity } });
  const central = await short.central(); assert(central.data.expiresAt <= Date.now() + 20000);
  const session = await short.login('alice', short.alpha.host, central); assert.equal(session.data.expiresAt, central.data.expiresAt);
  assert.match(session.headers['set-cookie']![0], /Max-Age=(?:19|20)$/);
  await short.advance(21000); assert.equal((await short.request(short.alpha.host, '/api/session', { cookies: session.cookies })).status, 401);
});

test('session: central bootstrap proof, exact Origin, session CSRF and duplicate cookies/JSON rejected', async t => {
  const f = await fixture(t), bootstrap = await f.request(accounts, '/auth/bootstrap');
  assert.equal((await f.request(accounts, '/auth/session', { body: {}, headers: { Authorization: 'Bearer alice' }, cookies: bootstrap.cookies })).status, 403);
  assert.equal((await f.request(accounts, '/auth/session', { body: {}, headers: { Authorization: 'Bearer alice' }, cookies: bootstrap.cookies, csrf: bootstrap.data.csrf, origin: origin(f.alpha.host) })).status, 403);
  const session = await f.login();
  for (const originValue of [null, 'null', origin(f.beta.host), 'https://evil.test']) {
    assert.equal((await f.request(f.alpha.host, '/api/sections', { body: { name: 'Design' }, cookies: session.cookies, csrf: session.data.csrf, origin: originValue })).status, 403);
  }
  assert.equal((await f.request(f.alpha.host, '/api/sections', { body: { name: 'Design' }, cookies: session.cookies })).status, 403);
  assert.equal((await f.request(f.alpha.host, '/api/sections', { rawBody: '{"name":"A","name":"B"}', cookies: session.cookies, csrf: session.data.csrf })).status, 400);
  assert.equal((await f.request(f.alpha.host, '/api/sections', { body: { name: 'Design' }, cookies: session.cookies, csrf: session.data.csrf, headers: { 'Content-Type': 'application/x-www-form-urlencoded' } })).status, 415);
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: [...session.cookies, session.cookies[0]] })).status, 401);
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: session.cookies, origin: origin(f.beta.host) })).status, 403);
  assert.equal((await f.request(f.alpha.host, '/api/sections', { body: { name: 'Design' }, cookies: session.cookies, csrf: session.data.csrf })).status, 200);
});

test('HTTP authority: Eve, foreign IDs and principal overrides denied; role checked on every request', async t => {
  const f = await fixture(t), eve = await f.login('eve'), alice = await f.login(), bob = await f.login('bob');
  assert.equal((await f.request(f.alpha.host, '/api/project', { cookies: eve.cookies })).status, 404);
  assert.equal((await f.request(f.alpha.host, `/api/history?sectionId=${f.beta.sectionId}`, { cookies: alice.cookies })).status, 404);
  assert.equal((await f.request(f.alpha.host, `/api/events?sectionId=${f.beta.sectionId}`, { cookies: alice.cookies })).status, 404);
  assert.equal((await f.request(f.alpha.host, `/api/turn?turnId=${randomUUID()}`, { cookies: alice.cookies })).status, 404);
  assert.equal((await f.request(f.alpha.host, '/api/messages', { body: { requestId: randomUUID(), sectionId: f.beta.sectionId, text: 'Foreign', mentions: [] }, cookies: alice.cookies, csrf: alice.data.csrf })).status, 404);
  assert.equal((await f.request(f.alpha.host, '/api/messages', { body: { requestId: randomUUID(), sectionId: f.alpha.sectionId, text: 'Forged', mentions: [], accountId: f.ids.bob }, cookies: alice.cookies, csrf: alice.data.csrf })).status, 400);
  assert.equal((await f.request(f.alpha.host, '/api/sections', { body: { name: 'Forbidden' }, cookies: bob.cookies, csrf: bob.data.csrf })).status, 403);
  await f.editMember(f.ids.bob, 1, 'viewer');
  assert.equal((await f.request(f.alpha.host, '/api/messages', { body: { requestId: randomUUID(), sectionId: f.alpha.sectionId, text: 'Denied', mentions: [] }, cookies: bob.cookies, csrf: bob.data.csrf })).status, 403);
  assert.equal((await f.request(f.alpha.host, '/api/history?sectionId=' + f.alpha.sectionId, { cookies: bob.cookies })).status, 200);
  await f.editMember(f.ids.bob, 2, 'revoked');
  assert.equal((await f.request(f.alpha.host, '/api/project', { cookies: bob.cookies })).status, 404);
});

test('SSE: removal closes affected stream, unrelated project stays open; account block closes every stream', async t => {
  const f = await fixture(t), parent = await f.central('bob'), alpha = await f.login('bob', f.alpha.host, parent), beta = await f.login('bob', f.beta.host, parent);
  const affected = await f.stream(alpha), unaffected = await f.stream(beta, f.beta.host, f.beta.sectionId);
  assert.equal(affected.res.headers['cache-control'], 'no-store'); assert.equal(affected.res.headers['access-control-allow-origin'], undefined);
  await f.editMember(f.ids.bob, 1, 'revoked'); await affected.closed;
  assert.equal(unaffected.res.readableEnded, false);
  f.db.prepare("UPDATE accounts SET status='blocked' WHERE id=?").run(f.ids.bob); await unaffected.closed;
});

test('SSE: verified expiry and restore generation close live streams and invalidate handoffs', async t => {
  const f = await fixture(t), parent = await f.central(), session = await f.login('alice', f.alpha.host, parent), stream = await f.stream(session);
  const started = await f.start(), code = await f.handoff(parent, started);
  f.db.prepare('UPDATE hub_state SET authority_generation=? WHERE id=1').run(randomUUID()); await stream.closed;
  assert.equal((await f.redeem(f.alpha.host, started, code)).status, 401);
  const fresh = await f.login(), expires = await f.stream(fresh); await f.advance(900001); await expires.closed;
});

test('logout: host-only versus global, browser sessions/codes/streams revoked, durable sid denial rejects fresh tokens', async t => {
  const f = await fixture(t), parent = await f.central('bob'), alpha = await f.login('bob', f.alpha.host, parent), beta = await f.login('bob', f.beta.host, parent);
  assert.equal((await f.request(f.alpha.host, '/auth/logout', { body: { global: false }, cookies: alpha.cookies, csrf: alpha.data.csrf })).data.state, 'host_revoked');
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: alpha.cookies })).status, 401);
  assert.equal((await f.request(f.beta.host, '/api/session', { cookies: beta.cookies })).status, 200);
  const stream = await f.stream(beta, f.beta.host, f.beta.sectionId), started = await f.start(), code = await f.handoff(parent, started);
  const logout = await f.request(accounts, '/auth/logout', { body: { global: true }, cookies: parent.cookies, csrf: parent.data.csrf });
  assert.equal(logout.data.state, 'upstream_pending'); await stream.closed;
  assert.equal((await f.redeem(f.alpha.host, started, code)).status, 401);
  assert.equal((await f.request(f.beta.host, '/api/session', { cookies: beta.cookies })).status, 401);
  const bootstrap = await f.request(accounts, '/auth/bootstrap');
  assert.equal((await f.request(accounts, '/auth/session', { body: {}, cookies: bootstrap.cookies, csrf: bootstrap.data.csrf, headers: { Authorization: 'Bearer bob' } })).status, 401);
  assert.equal(f.db.prepare('SELECT count(*) AS n FROM hub_browser_denials').get()!.n, 1);
});

test('hub routes: local-owner APIs denied on all hosts, no CORS, cache isolation and callback fragment policy', async t => {
  const f = await fixture(t);
  for (const host of [accounts, f.alpha.host, f.beta.host]) for (const path of ['/mcp', '/api/agents', '/api/settings', '/api/files', '/api/computer', '/api/releases', '/api/providers', '/api/update', '/api/machines', '/node', '/']) {
    const response = await f.request(host, path); assert.equal(response.status, 404, `${host}${path}`); assert.equal(response.headers['cache-control'], 'no-store'); assert.equal(response.headers['access-control-allow-origin'], undefined);
  }
  for (const host of [accounts, f.alpha.host, f.beta.host]) assert.equal((await f.request(host, `/api/agents/${randomUUID()}/delete`, { body: { id: randomUUID() } })).status, 404);
  assert.equal((await f.request(f.alpha.host, '/api/project', { method: 'OPTIONS', origin: origin(f.beta.host) })).status, 404);
  const callback = await f.request(f.alpha.host, '/auth/callback');
  assert.equal(callback.headers['referrer-policy'], 'no-referrer'); assert.equal(callback.headers['cache-control'], 'no-store');
  assert.match(String(callback.headers['content-security-policy']), /script-src 'sha256-/); assert.match(callback.raw, /history.replaceState/); assert(!callback.raw.includes('<script src='));
  assert.equal((await f.request(f.alpha.host, '/auth/redeem?code=leak', { body: {}, csrf: 'x' })).status, 401);
});

test('node upgrades accept only the exact central path and reject browser credentials or missing signer', async t => {
  const unmounted = await fixture(t), f = await fixture(t, { nodeLink: true });
  function upgrade(host: string, path = '/node', extra = '', hosted = f) {
    return hosted.raw('Host: ' + host + '\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==' + extra, path);
  }
  assert.match(await upgrade(accounts, '/node', '', unmounted), /^HTTP\/1.1 404/);
  assert.match(await upgrade(f.alpha.host), /^HTTP\/1.1 404/);
  assert.match(await upgrade(accounts, '/node?token=leak'), /^HTTP\/1.1 404/);
  assert.match(await upgrade(accounts, '/node', '\r\nOrigin: https://accounts.minarbot.test'), /^HTTP\/1.1 404/);
  assert.match(await upgrade(accounts, '/node', '\r\nCookie: __Host-session=leak'), /^HTTP\/1.1 404/);
  assert.match(await upgrade(accounts), /^HTTP\/1.1 403/);
});

const rsa = generateKeyPairSync('rsa', { modulusLength: 2048 });
const jwk = { ...rsa.publicKey.export({ format: 'jwk' }), kid: 'fixture-key', alg: 'RS256', use: 'sig' };
function jwt(claims: Record<string, unknown>, header: Record<string, unknown> = { alg: 'RS256', typ: 'JWT', kid: jwk.kid }, privateKey = rsa.privateKey) {
  const body = [header, claims].map(value => Buffer.from(JSON.stringify(value)).toString('base64url')).join('.');
  return `${body}.${sign('RSA-SHA256', Buffer.from(body), privateKey).toString('base64url')}`;
}
function claims(subject = 'alice') {
  const now = Math.floor(Date.now() / 1000);
  return { iss: issuer, sub: subject, sid: subject + '-jwt', azp: origin(accounts), iat: now - 5, nbf: now - 5, exp: now + 60,
    aud: 'minarbot-hub', email: subject + '@example.test', email_verified: true };
}

test('offline Clerk fixtures: valid RS256/JWKS, verified mailbox evidence and audience array', async t => {
  const f = await fixture(t, { clerk: true });
  const input = claims();
  const session = await f.central(jwt(input));
  const identity = JSON.parse(String(f.one("SELECT identity_json FROM hub_browser_sessions WHERE host='accounts.minarbot.test' AND account_id=(SELECT id FROM accounts WHERE subject='alice') ORDER BY rowid DESC LIMIT 1")!.identity_json));
  assert.deepEqual(identity, { issuer, subject: 'alice', sessionId: input.sid, expiresAt: input.exp * 1000, verifiedMailboxes: ['alice@example.test'] });
  const viewer = await f.request(accounts, '/api/session', { cookies: session.cookies });
  assert.equal(viewer.data.accountId, f.ids.alice);
  assert.equal((await f.authenticate(jwt({ ...input, aud: ['other', 'minarbot-hub'] }))).status, 200);
  await f.central(jwt({ ...input, email_verified: false }));
  const last = f.one('SELECT identity_json FROM hub_browser_sessions ORDER BY rowid DESC LIMIT 1');
  assert.deepEqual(JSON.parse(String(last!.identity_json)).verifiedMailboxes, []);
});

test('offline Clerk fixtures: expiry, issuer, azp, audience, time, sid, pending session and signature failures', async t => {
  const f = await fixture(t, { clerk: true });
  const input = claims(), now = input.exp - 60;
  for (const changed of [{ exp: now }, { iss: 'https://other.example.test' }, { azp: origin('alpha.minarbot.test') }, { aud: 'wrong' }, { aud: undefined }, { nbf: now + 30 }, { iat: now + 30 }, { sid: '' }, { sub: '' }, { sts: 'pending' }, { exp: String(input.exp) }, { nbf: undefined }]) {
    const result = await f.authenticate(jwt({ ...input, ...changed }));
    assert.equal(result.status, 401, JSON.stringify(changed));
    assert.equal(result.data.error.code, 'session_invalid');
  }
  const tokens = [
    jwt(input, { alg: 'HS256', kid: jwk.kid }),
    jwt(input, { alg: 'RS256', kid: 'unknown' }),
    jwt(input, { alg: 'RS256', kid: jwk.kid, crit: ['foo'] }),
    jwt(input, undefined, generateKeyPairSync('rsa', { modulusLength: 2048 }).privateKey),
    'not-a-jwt',
    jwt(input).split('.')[0] + '.' + Buffer.from('{"iss":"https://clerk.example.test","iss":"other"}').toString('base64url') + '.eA',
  ];
  for (const token of tokens) assert.equal((await f.authenticate(token)).status, 401);
});

test('offline Clerk config: HTTPS, key type, modulus, key IDs and no private JWKS', async t => {
  const dir = mkdtempSync(resolve(tmpdir(), 'minarbot-hub-clerk-'));
  t.after(() => rmSync(dir, { recursive: true }));
  const { config, jwksPath } = files(dir, {}, { clerk: true });
  assert(config.identity.mode === 'clerk');
  const binary = serverBinary();
  await assert.rejects(startRustHub(binary, dir, { ...config, identity: { ...config.identity, issuer: 'http://clerk.example.test' } }), /HTTPS/);
  const weak = generateKeyPairSync('rsa', { modulusLength: 1024 }).publicKey.export({ format: 'jwk' });
  for (const keys of [[jwk, jwk], [{ ...jwk, kty: 'EC' }], [{ ...weak, kid: 'weak' }], [{ ...jwk, d: 'private' }]]) {
    writeFileSync(jwksPath, JSON.stringify({ keys }));
    await assert.rejects(startRustHub(binary, dir, config), /pinned RSA/);
  }
});

test('real HTTP Clerk adapter: only central verified bearer admitted; token expiry bounds project cookie', async t => {
  const f = await fixture(t, { clerk: true });
  const input = claims(), token = jwt(input);
  const central = await f.central(token), session = await f.login('alice', f.alpha.host, central);
  assert.equal(session.data.expiresAt, input.exp * 1000);
  assert.equal((await f.request(f.alpha.host, '/api/project', { headers: { Authorization: 'Bearer ' + token } })).status, 401);
  assert.equal((await f.authenticate(jwt(claims('outside')))).status, 401);
});

test('documented starter: explicit isolated config, loopback binding, no automatic account provisioning', async t => {
  const dir = mkdtempSync(resolve(tmpdir(), 'minarbot-hub-start-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const { config } = files(dir, {}, {});
  const hub = await startRustHub(serverBinary(), dir, config);
  await hub.close();
  const db = testDatabase(config.dataPath, { readOnly: true });
  try { assert.equal(db.prepare('SELECT count(*) AS n FROM accounts').get()!.n, 0); }
  finally { db.close(); }
  assert(readFileSync(config.dataPath).length > 0);
});

test('session rotation and SQLite reopen preserve sessions/denials without replaying handoffs', async t => {
  const f = await fixture(t), parent = await f.central(), first = await f.login('alice', f.alpha.host, parent);
  const started = await f.start(), code = await f.handoff(parent, started);
  const rotated = await f.request(f.alpha.host, '/auth/redeem', { body: code, cookies: [...started.cookies, first.cookies[0]], csrf: started.data.state });
  assert.equal(rotated.status, 200);
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: first.cookies })).status, 401);
  assert(!f.db.prepare('SELECT digest FROM hub_browser_sessions').all().some(row => rotated.cookies[0].endsWith(String(row.digest))));
  await f.reopen();
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: rotated.cookies })).status, 200);
  assert.equal((await f.redeem(f.alpha.host, started, code)).status, 401);
  assert.equal((await f.request(accounts, '/auth/logout', { body: { global: true }, cookies: parent.cookies, csrf: parent.data.csrf })).status, 200);
  await f.reopen();
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: rotated.cookies })).status, 401);
  const bootstrap = await f.request(accounts, '/auth/bootstrap');
  assert.equal((await f.request(accounts, '/auth/session', { body: {}, cookies: bootstrap.cookies, csrf: bootstrap.data.csrf, headers: { Authorization: 'Bearer alice' } })).status, 401);
  assert.equal(f.one('SELECT count(*) AS n FROM hub_browser_denials')!.n, 1);
});

test('async JSON mutation rechecks current membership after slow request body', async t => {
  const f = await fixture(t), session = await f.login('bob');
  const data = JSON.stringify({ requestId: randomUUID(), sectionId: f.alpha.sectionId, text: 'Must not commit', mentions: [] });
  const req = httpRequest({ host: '127.0.0.1', port: f.port(), path: '/api/messages', method: 'POST', headers: { Host: f.alpha.host, Origin: origin(f.alpha.host), Cookie: session.cookies.join('; '), 'X-CSRF-Token': session.data.csrf, 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(data), Expect: '100-continue' } });
  t.after(() => req.destroy());
  const received = once(req, 'continue', { signal: AbortSignal.timeout(5000) });
  req.flushHeaders();
  await received;
  req.write(data.slice(0, 10));
  await f.editMember(f.ids.bob, 1, 'revoked');
  const response = once(req, 'response'); req.end(data.slice(10)); const [result] = await response as [IncomingMessage]; result.resume(); await once(result, 'end');
  assert.equal(result.statusCode, 404); assert.equal(f.one('SELECT count(*) AS n FROM messages')!.n, 0);
});

test('bounded HTTP JSON returns 413 for declared/chunked overflow and rejects normalized route aliases', async t => {
  const f = await fixture(t), session = await f.login();
  const result = await f.request(f.alpha.host, '/api/messages', { rawBody: JSON.stringify({ text: 'x'.repeat(65536) }), cookies: session.cookies, csrf: session.data.csrf });
  assert.equal(result.status, 413); assert.equal(result.headers['cache-control'], 'no-store');
  const req = httpRequest({ host: '127.0.0.1', port: f.port(), path: '/api/messages', method: 'POST', headers: { Host: f.alpha.host, Origin: origin(f.alpha.host), Cookie: session.cookies.join('; '), 'X-CSRF-Token': session.data.csrf, 'Content-Type': 'application/json', 'Transfer-Encoding': 'chunked' } });
  const response = once(req, 'response'); req.end(' '.repeat(65537)); const [chunked] = await response as [IncomingMessage]; chunked.resume(); await once(chunked, 'end');
  assert.equal(chunked.statusCode, 413);
  assert.equal((await f.request(f.alpha.host, '/api/../auth/start')).status, 404);
  assert.equal(f.one('SELECT count(*) AS n FROM messages')!.n, 0);
});

test('central renewal requires fresh exact-origin verification, rotates parent and fences old project sessions', async t => {
  const expiry = Date.now() + 30000;
  const identity = { issuer, subject: 'alice', sessionId: 'renewal-sid', verifiedMailboxes: [], expiresAt: expiry };
  const f = await fixture(t, { tokens: { initial: identity, fresh: { ...identity, expiresAt: expiry + 30000 } } });
  const parent = await f.central('initial'), child = await f.login('alice', f.alpha.host, parent), bootstrap = await f.request(accounts, '/auth/bootstrap');
  const options = { body: {}, cookies: [...bootstrap.cookies, parent.cookies[0]], csrf: bootstrap.data.csrf, headers: { Authorization: 'Bearer fresh' } };
  assert.equal((await f.request(accounts, '/auth/session', { ...options, origin: origin(f.alpha.host) })).status, 403);
  const renewed = await f.request(accounts, '/auth/session', options); assert.equal(renewed.status, 200); assert.equal(renewed.data.expiresAt, expiry + 30000);
  assert.equal((await f.request(f.alpha.host, '/api/session', { cookies: child.cookies })).status, 401);
  const next = await f.login('alice', f.alpha.host, renewed); assert.equal(next.data.expiresAt, expiry + 30000);
});


test('pilot web adapters: no outsider reads, exact mailbox acceptance, owner node/agent binding, role and participation policies', async t => {
  const f = await fixture(t, { omitInvite: true });
  const alice = await f.login(), bob = await f.login('bob'), eve = await f.login('eve');
  const post = (host: string, path: string, body: unknown, session: Response) =>
    f.request(host, path, { body, cookies: session.cookies, csrf: session.data.csrf });
  const view = (path: string, session = alice) => f.request(f.alpha.host, path, { cookies: session.cookies });
  for (const path of ['/api/members', '/api/invitations', '/api/participants', '/api/turns?sectionId=' + f.alpha.sectionId])
    assert.equal((await view(path, eve)).status, 404);

  const invite = await post(f.alpha.host, '/api/invitations', { mailbox: 'bob@example.test', role: 'member' }, alice);
  assert.equal(invite.status, 200, invite.raw);
  const proof = Object.fromEntries(new URLSearchParams(new URL(invite.data.url).hash.slice(1)));
  assert.equal((await post(f.alpha.host, '/api/invitations/accept', proof, eve)).status, 403);
  assert.equal((await post(f.alpha.host, '/api/invitations/accept', proof, bob)).status, 200);
  assert.equal((await post(f.alpha.host, '/api/invitations/accept', proof, bob)).status, 409);
  const invites = await view('/api/invitations');
  assert.equal(invites.data[0].state, 'accepted');
  assert(!invites.raw.includes(proof.token));
  assert.equal((await view('/api/members')).data.length, 2);
  assert.deepEqual((await view('/api/turns?sectionId=' + f.alpha.sectionId)).data, []);

  const nodeId = randomUUID();
  const spki = (kind: 'ed25519' | 'x25519') => {
    const keys = kind === 'ed25519' ? generateKeyPairSync('ed25519') : generateKeyPairSync('x25519');
    return keys.publicKey.export({ type: 'spki', format: 'der' }).toString('base64url');
  };
  f.db.prepare("INSERT INTO nodes(id,owner_account_id,installation_id,signing_public_key,encryption_public_key,capabilities_json) VALUES(?,?,?,?,?,?)")
    .run(nodeId, f.ids.bob, randomUUID(), spki('ed25519'), spki('x25519'), '["chat"]');
  const agent = { agentId: randomUUID(), nodeId, name: 'Bob agent' };
  const centralAlice = await f.central(), centralBob = await f.central('bob');
  assert.equal((await post(accounts, '/api/agents', agent, centralAlice)).status, 404);
  for (let attempt = 0; attempt < 2; attempt++)
    assert.equal((await post(accounts, '/api/agents', agent, centralBob)).status, 200);
  assert.equal((await post(accounts, '/api/agents', { ...agent, name: 'Changed' }, centralBob)).status, 403);

  const policy = { sectionIds: [f.alpha.sectionId], scopes: ['read', 'invoke'], invocation: { mode: 'all-members' }, payerBindingRef: 'local', localPolicyDigest: '1'.repeat(64), maxCostMicros: 100 };
  const proposal = await post(accounts, '/api/participations', { projectHost: f.alpha.host, agentId: agent.agentId, policy }, centralBob);
  assert.equal(proposal.status, 200, proposal.raw);
  const participant = (await view('/api/participants')).data[0];
  assert.equal(participant.id, proposal.data.participationId);
  assert.equal(participant.ownerName, 'bob');
  assert.equal(participant.nodeStatus, 'offline');
  const projectBob = await f.login('bob', f.alpha.host, centralBob);
  const projectAlice = await f.login('alice', f.alpha.host, centralAlice);
  assert.equal((await post(f.alpha.host, '/api/participants/consent', proposal.data, projectBob)).status, 403);
  assert.equal((await post(f.alpha.host, '/api/participants/consent', proposal.data, projectAlice)).status, 200);
  assert.equal((await view('/api/participants', projectAlice)).data[0].state, 'proposed');
  assert.equal((await post(f.alpha.host, '/api/invitations', { mailbox: 'eve@example.test', role: 'admin' }, projectBob)).status, 403);
});
