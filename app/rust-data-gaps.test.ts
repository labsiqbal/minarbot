import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, randomBytes, randomUUID, sign } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:http';
import { testDatabase } from './test-support/database.ts';
import { installCanonicalFixture } from './test-support/canonical-schema.ts';
import { startRustServer } from './rust-test-server.ts';
import { startApp } from './test-bridge.ts';
import { decryptMail } from './test-support/mail-fixture.ts';
import { digestCommand } from './shared/protocol.ts';
import sharp from 'sharp';
import { execFileSync } from 'node:child_process';
import { legacyCheckpointFixture as createWorkspaceGit } from './test-support/node-checkpoint-fixture.ts';

async function fixture() {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-data-gaps-')), database = join(directory, 'db.sqlite');
  const keys = generateKeyPairSync('rsa', { modulusLength: 2048 }), key = randomBytes(32), lookupKey = randomBytes(32), keyId = randomUUID();
  let mailbox = 'member@example.com', locked = false;
  const upstream = createServer((req, res) => { res.setHeader('Content-Type', 'application/json'); res.statusCode = locked ? 503 : 200; res.end(JSON.stringify({ id: 'member', email_addresses: [{ email_address: mailbox, verification: { status: 'verified' } }, { email_address: 'wrong@example.com', verification: { status: 'unverified' } }] })); });
  await new Promise<void>(ready => upstream.listen(0, '127.0.0.1', ready));
  const address = upstream.address(); assert(address && typeof address !== 'string');
  const auth = { secretKey: 'sk_test_fixture', publishableKey: 'pk_test_fixture', issuer: 'https://fixture.clerk.accounts.dev', origin: 'https://minarbot.example', development: true, jwtKey: keys.publicKey.export({ type: 'spki', format: 'pem' }).toString(), apiUrl: `http://127.0.0.1:${address.port}` };
  const db = testDatabase(database); installCanonicalFixture(db);
  const owner = randomUUID(), member = randomUUID();
  for (const [id, subject] of [[owner, 'owner'], [member, 'member']]) db.prepare('INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES(?,?,?,?)').run(id, auth.issuer, subject, Date.now());
  db.close();
  const options = { database, auth, testKeyringLocked: false, mailFrom: 'Minarbot <noreply@example.com>', testKeyring: { 'mail-aead/active': keyId, [`mail-aead/${keyId}`]: key.toString('base64url'), 'mail-lookup/v1': lookupKey.toString('base64url'), 'sync-hmac/v1': randomBytes(32).toString('base64url') } };
  let app = await startApp(options, { directory, servers: [upstream] });
  const token = (sub: string) => { const now = Math.floor(Date.now() / 1000), unsigned = `${Buffer.from(JSON.stringify({ alg: 'RS256', typ: 'JWT' })).toString('base64url')}.${Buffer.from(JSON.stringify({ iss: auth.issuer, sub, sid: `sess_${sub}`, azp: auth.origin, nbf: now - 1, exp: now + 600, iat: now - 1 })).toString('base64url')}`; return `${unsigned}.${sign('RSA-SHA256', Buffer.from(unsigned), keys.privateKey).toString('base64url')}`; };
  async function request(path: string, body?: Record<string, unknown>, status = 200, subject = 'owner', method = 'POST'): Promise<any> {
    const response = await fetch(app.origin + path, { method: body ? method : 'GET', headers: { Authorization: `Bearer ${token(subject)}`, Origin: auth.origin, 'Content-Type': 'application/json', 'X-Minarbot-Protocol': '1', 'X-Minarbot-Request-Id': String(body?.requestId ?? randomUUID()) }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert.equal(response.status, status, JSON.stringify(value)); return value;
  }
  return { database, owner, member, key, keyId, request, keyring(locked: boolean) { options.testKeyringLocked = locked; }, mailbox(value: string) { mailbox = value; }, lock(value: boolean) { locked = value; }, async restart() { await app.close(); app = await startRustServer(options); }, async close() { await app.close(); upstream.closeAllConnections(); await new Promise<void>(ready => upstream.close(() => ready())); rmSync(directory, { recursive: true, force: true }); } };
}

test('Rust invitation ciphertext opens in Node; verified mailbox, receipts, revoke and restart stay scoped', async () => {
  const f = await fixture();
  try {
    const project = randomUUID(); await f.request('/api/projects', { requestId: randomUUID(), id: project, name: 'Invitation fixture' }, 201);
    const command = { requestId: randomUUID(), email: 'MEMBER@Example.com', agentAssignments: [], expiresAt: Date.now() + 60000, expectedProjectRevision: 1 };
    const path = `/api/projects/${project}/invitations`;
    const created = await f.request(path, command, 201); assert.equal(created.data.state, 'pending_delivery');
    assert.deepEqual((await f.request(path, command)).receipt, created.receipt);
    await f.request(path, { ...command, requestId: randomUUID() }, 409);
    const db = testDatabase(f.database), invitation = db.prepare('SELECT * FROM invitations WHERE id=?').get(command.requestId)!, outbox = db.prepare('SELECT * FROM mail_outbox WHERE id=?').get(command.requestId)!; db.close();
    const decrypted = decryptMail(command.requestId, 1, String(invitation.authority_generation), String(invitation.email_hash), outbox.ciphertext as Uint8Array, f.key);
    assert.equal(decrypted.payload.to, 'member@example.com'); assert.equal(decrypted.payloadDigest, outbox.payload_digest);
    const mailboxProof = decrypted.payload.text.match(/#([A-Za-z0-9_-]{43})/)![1], accept = { requestId: randomUUID(), mailboxProof, expectedRevision: 1 };
    f.lock(true); await f.request(`/api/invitations/${command.requestId}/accept`, accept, 503, 'member'); f.lock(false);
    f.mailbox('wrong@example.com'); await f.request(`/api/invitations/${command.requestId}/accept`, accept, 404, 'member'); f.mailbox('member@example.com');
    const accepted = await f.request(`/api/invitations/${command.requestId}/accept`, accept, 200, 'member'); assert.equal(accepted.data.grantRevision, 1);
    f.lock(true); await f.restart(); assert.deepEqual((await f.request(`/api/invitations/${command.requestId}/accept`, accept, 200, 'member')).receipt, accepted.receipt); f.lock(false);
    await f.request(`/api/invitations/${command.requestId}/revoke`, { requestId: randomUUID(), expectedRevision: 2 }, 409);
    f.keyring(true); await f.restart();
    assert.equal((await f.request(path, { ...command, requestId: randomUUID(), email: 'locked@example.com' }, 503)).error.code, 'mail_key_unavailable');
    assert.equal((await f.request(`/api/sync?projectId=${project}`, undefined, 503)).error.code, 'keystore_unavailable');
    f.keyring(false); await f.restart();
    const second = { ...command, requestId: randomUUID(), email: 'second@example.com' }; await f.request(path, second, 201);
    const revoke = { requestId: randomUUID(), expectedRevision: 1 }; await f.request(`/api/invitations/${second.requestId}/revoke`, revoke); await f.request(`/api/invitations/${second.requestId}/revoke`, revoke);
    const check = testDatabase(f.database); assert(check.prepare('SELECT length(ciphertext) n FROM mail_outbox').all().every(row => row.n === 0)); check.close();
  } finally { await f.close(); }
});

test('Rust sync snapshots page, hash and replay; delta cursors survive restart and reject tampering', async () => {
  const f = await fixture();
  try {
    const project = randomUUID(); await f.request('/api/projects', { requestId: randomUUID(), id: project, name: 'First' }, 201);
    const snapshot = (await f.request(`/api/sync?projectId=${project}`)).data; assert.equal(snapshot.mode, 'snapshot'); assert.equal(snapshot.snapshotHash, digestCommand(snapshot.page.events));
    const change = { requestId: randomUUID(), expectedRevision: 1, name: 'Second' }; await f.request(`/api/projects/${project}`, change, 200, 'owner', 'PATCH'); await f.request(`/api/projects/${project}`, change, 200, 'owner', 'PATCH');
    await f.restart(); const delta = (await f.request(`/api/sync?projectId=${project}&cursor=${snapshot.nextCursor}`)).data;
    assert.equal(delta.page.events.length, 1); assert.equal(delta.page.events[0].body.name, 'Second'); assert.equal(delta.mode, 'delta');
    assert.deepEqual((await f.request(`/api/sync?projectId=${project}&cursor=${delta.nextCursor}`)).data.page.events, []);
    await f.request(`/api/sync?projectId=${project}&cursor=broken`, undefined, 400); await f.request(`/api/sync?projectId=${project}&cursor=${snapshot.nextCursor}x`, undefined, 400);
    await f.request(`/api/sync?projectId=${project}&projectId=${project}`, undefined, 400);
    const db = testDatabase(f.database); db.exec('BEGIN');
    for (let n = 0; n < 1002; n++) { const id = randomUUID(); db.prepare('INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES(?,?,?,?)').run(id, 'fixture', id, Date.now()); db.prepare("INSERT INTO project_members VALUES(?,?,'member',1,0)").run(project, id); } db.exec('COMMIT'); db.close();
    const pages = [], events = []; let page = (await f.request(`/api/sync?projectId=${project}`)).data;
    for (;;) { pages.push(page); events.push(...page.page.events); if (!page.nextPage) break; const path = `/api/sync?projectId=${project}&page=${page.nextPage}`; page = (await f.request(path)).data; assert.deepEqual((await f.request(path)).data, page); }
    assert.equal(pages.length, 3); assert.equal(page.snapshotHash, digestCommand(events));
    await f.request(`/api/sync?projectId=${project}&page=${pages[0].nextPage}`, undefined, 409);
    const opened = (await f.request(`/api/sync?projectId=${project}`)).data;
    const authority = testDatabase(f.database); authority.prepare('UPDATE accounts SET revision=revision+1 WHERE id=?').run(f.owner); authority.close();
    await f.request(`/api/sync?projectId=${project}&page=${opened.nextPage}`, undefined, 410);
    await f.request(`/api/sync?projectId=${project}&cursor=${delta.nextCursor}`, undefined, 410);
    const revoked = testDatabase(f.database); revoked.prepare('UPDATE project_members SET deleted=1 WHERE project=? AND account=?').run(project, f.owner); revoked.close();
    await f.request(`/api/sync?projectId=${project}`, undefined, 403);
  } finally { await f.close(); }
});

test('Rust avatars preserve decoded pixels across resizing, alpha, JPEG, WebP and orientation', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-avatar-parity-'));
  const app = await startRustServer({ database: join(directory, 'db.sqlite') });
  const request = async (path: string, body?: unknown, status = 200) => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const result = await response.json(); assert.equal(response.status, status, JSON.stringify(result)); return result;
  };
  try {
    const agent = await request('/api/agents', { id: randomUUID(), name: 'Avatar', model: 'fixture', instructions: 'Help' }, 201);
    let revision = 0;
    for (const [width, height, channels, format, orientation] of [[513, 257, 4, 'png', 1], [73, 901, 3, 'png', 1], [512, 393, 3, 'jpeg', 1], [320, 241, 4, 'webp', 1], [256, 256, 3, 'png', 1], [17, 17, 1, 'png', 1], [43, 67, 3, 'jpeg', 6], [31, 23, 4, 'png', 3]] as const) {
      const raw = Buffer.alloc(width * height * channels);
      for (let i = 0; i < raw.length; i++) {
        const x = Math.floor(i / channels) % width, y = Math.floor(i / channels / width), c = i % channels;
        raw[i] = width === 256 && height === 256 ? (i * 37 + (i >>> 8) * 13) % 256
          : c === 0 ? 30 + Math.floor(180 * x / width) : c === 1 ? 40 + Math.floor(160 * y / height)
          : c === 2 ? 70 + Math.floor(60 * (x / width + y / height)) : 200;
      }
      const input = await sharp(raw, { raw: { width, height, channels } }).withMetadata({ orientation }).toFormat(format, format === 'jpeg' ? { quality: 70 } : undefined).toBuffer();
      const expected = await sharp(input).rotate().resize(256, 256, { fit: 'cover' }).png().toBuffer();
      await request(`/api/agents/${agent.id}/avatar`, { id: randomUUID(), revision: revision++, image: input.toString('base64') });
      const response = await fetch(`${app.origin}/api/agents/${agent.id}/avatar/image`); assert.equal(response.status, 200);
      const actual = Buffer.from(await response.arrayBuffer());
      const meta = await sharp(actual).metadata(), reference = await sharp(expected).metadata();
      assert.equal(meta.width, reference.width); assert.equal(meta.height, reference.height);
      assert.equal(meta.format, reference.format); assert.equal(meta.channels, reference.channels);
      assert.equal(meta.space, reference.space); assert.equal(meta.depth, reference.depth);
      const pixels = await sharp(actual).raw().toBuffer(), expectedPixels = await sharp(expected).raw().toBuffer();
      const tolerance = width === 256 && height === 256 ? 0 : format === 'png' ? 4 : format === 'webp' ? 8 : 5;
      assert.equal(pixels.length, expectedPixels.length);
      for (let i = 0; i < pixels.length; i++) assert(Math.abs(pixels[i] - expectedPixels[i]) <= tolerance,
        `${width}x${height} ${format} orientation ${orientation}: pixel ${i}, ${pixels[i]} vs ${expectedPixels[i]}, tolerance ${tolerance}`);
    }
  } finally { await app.close(); rmSync(directory, { recursive: true, force: true }); }
});

test('Rust workspace and sidebar preserve UTF-16 slices; script grants replay and visibility skips are durable', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-wire-parity-')), database = join(directory, 'db.sqlite'), agent = randomUUID();
  const options = { database, fixedClock: Date.now(), scriptToken: 'fixture-script-token-abcdefghijklmnopqrstuvwxyz', workspaces: [{ agent, directory, write: true }] };
  let app = await startRustServer(options);
  const request = async (path: string, body?: unknown, status = 200) => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const result = await response.json(); assert.equal(response.status, status, JSON.stringify(result)); return result;
  };
  try {
    await request('/api/agents', { id: agent, name: 'Wire', model: 'fixture', instructions: 'Help' }, 201);
    const text = 'x'.repeat(7999) + '😀tail'; writeFileSync(join(directory, 'astral.txt'), text);
    const file = `/api/agents/${agent}/workspace/file?path=astral.txt`;
    assert.equal((await request(file)).text, text.slice(0, 8000)); assert.equal((await request(file + '&offset=8000')).text, text.slice(8000));
    const db = testDatabase(database), turn = randomUUID();
    db.prepare("INSERT INTO turns(id,agent,text,status,reply,created) VALUES(?,?,?,'done',?,?)").run(turn, agent, '', 'x'.repeat(179) + '😀tail', new Date().toISOString());
    const summary = (await request('/api/sidebar/summary')).summaries.find((row: { agent: string }) => row.agent === agent);
    assert.equal(summary?.preview, ('x'.repeat(179) + '😀tail').slice(0, 180));
    const child = randomUUID(), delivery = randomUUID(), thread = await request(`/api/agents/${agent}/threads`, { id: randomUUID(), name: 'Reply' });
    const reply = 'x'.repeat(999) + '😀tail';
    db.prepare("INSERT INTO turns(id,agent,thread,text,status,reply,created) VALUES(?,?,?,'Task','done',?,?)").run(child, agent, thread.id, reply, new Date().toISOString());
    db.prepare('INSERT INTO agent_requests VALUES(?,?,?,1,0)').run(delivery, turn, 'fixture');
    db.prepare('INSERT INTO agent_deliveries VALUES(?,?)').run(delivery, child);
    db.prepare('INSERT INTO agent_reply_claims VALUES(?,?,?)').run(child, turn, JSON.stringify({ id: child, reply, truncated: false }));
    const history = await request(`/api/agents/${agent}/turns`);
    assert.equal(history[0].agentMessages[0].reply, reply.slice(0, 1000));
    assert.equal(history[0].receivedAgentReplies[0].reply, reply.slice(0, 1000));
    assert.equal(history[0].receivedAgentReplies[0].truncated, true);
    const long = 'x'.repeat(9999) + '😀' + 'x'.repeat(9999);
    db.prepare('UPDATE turns SET text=? WHERE id=?').run(long, turn);
    const fork = await request(`/api/agents/${agent}/turns/${turn}/fork`, { id: randomUUID() });
    const transcript = JSON.parse(String(db.prepare('SELECT transcript FROM context_transfers WHERE target_thread=?').get(fork.id)?.transcript));
    assert.equal(transcript.turns.at(-1).text, long.slice(0, 10000)); assert.equal(transcript.truncated, true);
    assert.deepEqual(await request(`/api/threads/${fork.id}/fork-context`), transcript);
    assert.deepEqual(await request(`/api/agents/${agent}/scripts`), { enabled: 0, revision: 0, configured: true });
    const grant = { id: randomUUID(), revision: 0, enabled: true }; assert.deepEqual(await request(`/api/agents/${agent}/scripts`, grant), { enabled: 1, revision: 1 });
    assert.deepEqual(await request(`/api/agents/${agent}/scripts`, grant), { enabled: 1, revision: 1 });
    await request(`/api/agents/${agent}/scripts`, { ...grant, enabled: false }, 409);
    await request(`/api/agents/${agent}/visibility`, { id: randomUUID(), revision: 0, hidden: true });
    const due = options.fixedClock - 180000, routine = randomUUID(), workflow = randomUUID();
    db.prepare("INSERT INTO routines(id,agent,name,prompt,next_at,interval_ms,enabled) VALUES(?,?,'Skip','Skip',?,60000,1)").run(routine, agent, due);
    db.prepare("INSERT INTO workflows VALUES(?,?,'Skip',1,0)").run(workflow, agent);
    db.prepare("INSERT INTO workflow_schedules VALUES(?,1,?,NULL,'Skip',1)").run(workflow, due);
    const restore = { id: randomUUID(), revision: 1, hidden: false }; await request(`/api/agents/${agent}/visibility`, restore); await request(`/api/agents/${agent}/visibility`, restore);
    assert.equal(db.prepare('SELECT missed FROM agent_visibility_skips WHERE routine=?').get(routine)?.missed, 3);
    assert.equal(db.prepare('SELECT next_at FROM routines WHERE id=?').get(routine)?.next_at, options.fixedClock + 60000);
    assert.deepEqual({ ...db.prepare('SELECT enabled,revision FROM workflow_schedules WHERE workflow=?').get(workflow) }, { enabled: 0, revision: 2 });
    assert.equal(db.prepare('SELECT reason FROM workflow_occurrences WHERE workflow=?').get(workflow)?.reason, 'hidden'); db.close();
    await app.close(); app = await startRustServer(options); assert.equal((await request(`/api/agents/${agent}/scripts`)).enabled, 1);
  } finally { await app.close(); rmSync(directory, { recursive: true, force: true }); }
});

test('Rust checkpoint project bindings use the turn agent grant and preserve panel file access', async () => {
  const directory = mkdtempSync(join(process.env.HOME!, 'minarbot-project-checkpoint-')), database = join(directory, 'data/db.sqlite'), workspace = join(directory, 'workspace');
  mkdirSync(join(directory, 'data')); mkdirSync(workspace); writeFileSync(join(workspace, 'note.txt'), 'Checkpoint');
  execFileSync('git', ['init', '-q', workspace]);
  const app = await startRustServer({ database });
  const request = async (path: string, body?: unknown, status = 200) => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert.equal(response.status, status, JSON.stringify(value)); return value;
  };
  try {
    const first = randomUUID(), agent = randomUUID();
    for (const id of [first, agent]) await request('/api/agents', { id, name: 'Fixture', model: 'fixture', instructions: 'Help' }, 201);
    const project = await request('/api/projects', { id: randomUUID(), name: 'Project', folders: [{ mode: 'existing', path: workspace }], members: [{ agent: first, write: false }, { agent, write: true }], confirmWrite: true });
    const db = testDatabase(database), turn = randomUUID();
    db.prepare("INSERT INTO turns(id,agent,channel,text,status,created) VALUES(?,?,?,'Fixture','done',?)").run(turn, agent, project.channel, new Date().toISOString());
    const git = createWorkspaceGit(db, new Map([[agent, { directory: workspace, write: true }]]), directory);
    await git.capture(agent, turn); writeFileSync(join(workspace, 'note.txt'), 'Changed');
    const preview = await request(`/api/agents/${agent}/git/checkpoints/${turn}`);
    assert.deepEqual(preview, await git.preview(agent, turn));
    const id = randomUUID(), path = `/api/channels/${project.channel}/files/${id}`;
    const upload = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/octet-stream', 'X-File-Name': 'history.txt' }, body: 'History upload' });
    assert.equal(upload.status, 200);
    assert.equal((await request(`/api/channels/${project.channel}/panel/files/${id}?metadata=1`)).name, 'history.txt');
    assert.equal((await request(`/api/channels/${project.channel}/panel/workspace/file?path=note.txt`)).text, 'Changed');
    const current = await request(`/api/projects/${project.id}`);
    await request(`/api/projects/${project.id}/edit`, { id: randomUUID(), revision: current.revision, members: [{ agent: first, write: false }] });
    await request(`/api/agents/${agent}/git/checkpoints/${turn}`, undefined, 403); db.close();
  } finally { await app.close(); rmSync(directory, { recursive: true, force: true }); }
});
