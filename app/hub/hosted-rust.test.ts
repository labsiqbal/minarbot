import assert from 'node:assert/strict';
import { generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { mkdtempSync, readFileSync, statSync, writeFileSync, rmSync } from 'node:fs';
import { request } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { rustBinary } from '../rust-binary.ts';
import { testDatabase } from '../test-support/database.ts';
import { hubCommand, startRustHub } from '../test-support/hosted-hub.ts';
import { signedNodeMessage, verifiedNodeMessage } from './node-link.ts';

test('hosted Rust reopens the frozen Hub fixture, verifies offline identity and fences durable session and peer proofs', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-hosted-rust-'));
  let hub: Awaited<ReturnType<typeof startRustHub>> | undefined;
  t.after(async () => {
    await hub?.close();
    rmSync(dir, { recursive: true });
  });
  const binary = rustBinary('minarbot-server', process.env.MINARBOT_HOSTED_HUB_BINARY ?? process.env.MINARBOT_RUST_BINARY);
  const domain = 'minarbot.test', accountsHost = 'accounts.' + domain;
  const authorityOrigin = 'https://' + accountsHost, issuer = 'https://identity.test';
  const dataPath = join(dir, 'hub.sqlite'), mailboxKeyPath = join(dir, 'mailbox.key');
  const jwksPath = join(dir, 'jwks.json'), signingKeyPath = join(dir, 'signer.pem');
  const mailboxKey = Buffer.alloc(32, 12);
  const identity = generateKeyPairSync('rsa', { modulusLength: 2048 });
  const signer = generateKeyPairSync('ed25519'), nodeKeys = generateKeyPairSync('ed25519');
  const encryption = generateKeyPairSync('x25519');
  const publicKey = (k: typeof signer.publicKey) => k.export({ type: 'spki', format: 'der' }).toString('base64url');
  writeFileSync(mailboxKeyPath, mailboxKey, { mode: 0o600 });
  writeFileSync(jwksPath, JSON.stringify({ keys: [{ ...identity.publicKey.export({ format: 'jwk' }), kid: 'fixture', alg: 'RS256', use: 'sig' }] }), { mode: 0o600 });
  writeFileSync(signingKeyPath, signer.privateKey.export({ type: 'pkcs8', format: 'pem' }), { mode: 0o600 });
  const subject = 'pilot';
  const account = randomUUID(), nodeId = randomUUID(), generation = randomUUID();
  const project = { id: randomUUID(), host: `pilot.${domain}` };
  writeFileSync(dataPath, '', { mode: 0o600 });
  const db = testDatabase(dataPath);
  try {
    db.exec(readFileSync(new URL('../test-support/fixtures/hub-pre-collapse.sql', import.meta.url), 'utf8'));
    db.exec('BEGIN');
    db.prepare("INSERT INTO hub_state(id,authority_generation,state) VALUES(1,?,'active')").run(generation);
    db.prepare("INSERT INTO accounts(id,issuer,subject,status,pilot_admitted) VALUES(?,?,?,'active',1)").run(account, issuer, subject);
    db.prepare("INSERT INTO projects(id,owner_account_id,slug,name) VALUES(?,?,'pilot','Fixture pilot')").run(project.id, account);
    db.prepare("INSERT INTO memberships(project_id,account_id,role) VALUES(?,?,'owner')").run(project.id, account);
    db.prepare('INSERT INTO host_bindings(host,project_id) VALUES(?,?)').run(project.host, project.id);
    db.prepare("INSERT INTO sections(id,project_id,name) VALUES(?,?,'general')").run(randomUUID(), project.id);
    db.prepare('INSERT INTO nodes(id,owner_account_id,installation_id,signing_public_key,encryption_public_key,capabilities_json) VALUES(?,?,?,?,?,?)')
      .run(nodeId, account, randomUUID(), publicKey(nodeKeys.publicKey), publicKey(encryption.publicKey), '["chat"]');
    db.exec('COMMIT');
  } finally { db.close(); }
  const config = { dataPath, mailboxKeyPath, accountsHost, domain, identity: { mode: 'clerk' as const, issuer, jwksPath, audience: 'hub-fixture' }, nodeLink: { signingKeyPath } };
  hub = await startRustHub(binary, dir, config);
  const send = (host: string, path: string, body?: unknown, headers: Record<string, string> = {}, browser = true) => new Promise<{ status: number; value: Record<string, unknown>; cookie: string }>((resolve, reject) => {
    const req = request({ host: '127.0.0.1', port: hub?.port, path, method: body === undefined ? 'GET' : 'POST',
      headers: { Host: host, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }), ...(browser && body !== undefined ? { Origin: 'https://' + host } : {}), ...headers },
      timeout: 5000,
    }, res => {
      let raw = '';
      res.setEncoding('utf8').on('data', chunk => { raw += chunk; }).on('end', () => {
        try { resolve({ status: res.statusCode ?? 0, value: JSON.parse(raw), cookie: res.headers['set-cookie']?.[0]?.split(';')[0] ?? '' }); }
        catch (error) { reject(error); }
      });
    });
    req.on('timeout', () => req.destroy(new Error('Fixture request timed out'))).on('error', reject);
    req.end(body === undefined ? undefined : JSON.stringify(body));
  });
  const jwt = (claimOverrides: Record<string, unknown> = {}) => {
    const sec = Math.floor(Date.now() / 1000);
    const encode = (v: unknown) => Buffer.from(JSON.stringify(v)).toString('base64url');
    const data = encode({ alg: 'RS256', kid: 'fixture' }) + '.' + encode({
      iss: issuer, sub: subject, sid: 'synthetic-sid', azp: authorityOrigin, aud: ['hub-fixture'],
      iat: sec - 1, nbf: sec - 1, exp: sec + 300, ...claimOverrides,
    });
    return data + '.' + sign('RSA-SHA256', Buffer.from(data), identity.privateKey).toString('base64url');
  };
  const string = (value: unknown) => {
    assert.equal(typeof value, 'string');
    return String(value);
  };
  const login = async (token: string) => {
    const boot = await send(accountsHost, '/auth/bootstrap');
    assert.equal(boot.status, 200);
    return send(accountsHost, '/auth/session', {}, { Cookie: boot.cookie, 'X-CSRF-Token': string(boot.value.csrf), Authorization: 'Bearer ' + token });
  };
  assert.equal((await login(jwt({ aud: 'other-audience' }))).status, 401);
  assert.equal((await login(jwt({ sub: 'outside' }))).status, 401);
  const central = await login(jwt());
  assert.equal(central.status, 200);
  const start = await send(project.host, '/auth/start');
  assert.equal(start.status, 200);
  const handoff = await send(accountsHost, '/auth/handoff', { state: start.value.state }, { Cookie: central.cookie, 'X-CSRF-Token': string(central.value.csrf) });
  assert.equal(handoff.status, 200);
  const fragment = new URLSearchParams(new URL(string(handoff.value.callback)).hash.slice(1));
  const code = Object.fromEntries(fragment);
  const redeem = () => send(project.host, '/auth/redeem', code, { Cookie: start.cookie, 'X-CSRF-Token': string(start.value.state) });
  const redeemed = await Promise.all([redeem(), redeem()]);
  assert.deepEqual(redeemed.map(result => result.status).sort(), [200, 401]);
  const projectCookie = redeemed.find(result => result.status === 200)?.cookie ?? '';
  assert.equal((await send(accountsHost, '/api/session', undefined, { Cookie: projectCookie })).status, 401);

  const clock = Date.now();
  const proof = signedNodeMessage({
    kind: 'session.renew', nodeId, nodeGeneration: 1, authorityGeneration: generation, origin: authorityOrigin,
    nonce: Buffer.alloc(32, 19).toString('base64url'), issuedAt: clock, expiresAt: clock + 60_000,
  }, nodeKeys.privateKey, 'node');
  const renew = (browser = false) => send(accountsHost, '/api/nodes/session', { nodeId, proof }, {}, browser);
  assert.equal((await renew(true)).status, 403);
  const session = await renew();
  assert.equal(session.status, 200);
  const verified = verifiedNodeMessage(session.value, publicKey(signer.publicKey), 'hub');
  assert.equal(verified.kind, 'session.renewed');

  await hub.close();
  hub = await startRustHub(binary, dir, config);
  assert.equal((await send(project.host, '/api/session', undefined, { Cookie: projectCookie })).status, 200);
  assert.equal((await redeem()).status, 401);
  assert.equal((await renew()).status, 403);
  const logout = await send(accountsHost, '/auth/logout', { global: true }, { Cookie: central.cookie, 'X-CSRF-Token': string(central.value.csrf) });
  assert.equal(logout.value.state, 'upstream_pending');
  assert.equal((await login(jwt())).status, 401);
  await hub.close();

  const copy = join(dir, 'restore.sqlite');
  hubCommand(binary, dir, config, 'backup', copy);
  assert.equal(statSync(copy).mode & 0o777, 0o600);
  const restored = { ...config, dataPath: copy };
  const recovery = JSON.parse(hubCommand(binary, dir, restored, 'fence-restore'));
  assert.notEqual(recovery.authorityGeneration, generation);
  assert.equal(recovery.state, 'recovery');
  hubCommand(binary, dir, restored, 'activate', recovery.authorityGeneration);
  hub = await startRustHub(binary, dir, restored);
  assert.equal((await login(jwt())).status, 401, 'restored database must preserve session denial');
  assert.equal((await login(jwt({ sid: 'post-restore-sid' }))).status, 200);
});
