import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, sign, randomBytes, randomUUID } from 'node:crypto';
import type { DatabaseSync } from 'node:sqlite';
import { testDatabase } from './test-support/database.ts';
import { chmodSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { installCanonicalFixture } from './test-support/canonical-schema.ts';
import { checkedRestore, restoreCommand, workspaceSnapshot } from './test-support/backup.ts';
import { restoreIdentity } from './test-support/restore-identity.ts';
import type { HumanActor } from './test-support/auth.ts';
import { canonicalJson, sha256 } from './shared/protocol.ts';

type FenceTrust = { v: 1; endpoint: string; controllerId: string; targetId: string; signingPublicKey: string };
type FenceRequest = {
  v: 1; requestId: string; challenge: string; endpoint: string; controllerId: string; targetId: string;
  sourceAuthority: string; recoveryGeneration: string; ownerAccount: string; createdAt: number; expiresAt: number;
};
type FenceEvidence = {
  request: FenceRequest; fenceId: string; processFenced: true; routeFenced: true; admissionsFenced: true;
  observedAt: number; expiresAt: number; signature: string;
};
function fenceEvidenceBody(evidence: Omit<FenceEvidence, 'signature'>) {
  return ['minarbot-infrastructure-fence-v1', evidence.request, evidence.fenceId, evidence.processFenced, evidence.routeFenced, evidence.admissionsFenced, evidence.observedAt, evidence.expiresAt];
}
function holdRestoreGuard(database: string) {
  const path = database + '.server-lock.sqlite';
  const guard = testDatabase(path, { timeout: 0 });
  chmodSync(path, 0o600);
  guard.exec('BEGIN EXCLUSIVE');
  return guard;
}

async function fixture(seed?: (db: DatabaseSync, owner: HumanActor, project: string) => void) {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-fence-')), database = join(dir, 'restored.sqlite');
  let db = testDatabase(join(dir, 'source.sqlite')); installCanonicalFixture(db);
  const owner: HumanActor = { accountId: '11111111-1111-4111-8111-111111111111', issuer: 'https://fixture.clerk.accounts.dev', subject: 'user_owner', sessionId: 'sess_owner', expiresAt: Date.now() + 300000 };
  const config = { origin: 'https://minarbot.example', issuer: owner.issuer, secretKey: 'sk_test_fixture', publishableKey: `pk_test_${Buffer.from('fixture.clerk.accounts.dev$').toString('base64')}` };
  const project = '22222222-2222-4222-8222-222222222222';
  db.prepare('INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES (?,?,?,?)').run(owner.accountId, owner.issuer, owner.subject, Date.now());
  db.prepare('INSERT INTO projects(id,account,name,private,deleted,revision) VALUES (?,?,?,1,0,1)').run(project, owner.accountId, 'Restore');
  db.prepare("INSERT INTO project_members(project,account,role,grant_revision,deleted) VALUES (?,?,'owner',1,0)").run(project, owner.accountId);
  seed?.(db, owner, project);
  const snapshot = await workspaceSnapshot(db); db.close();
  let bootstrapFile: string;
  try { bootstrapFile = checkedRestore(['--read-only', snapshot.file, '--db', database]).bootstrapFile; } finally { snapshot.cleanup(); }
  db = testDatabase(database);
  const identity = restoreIdentity(owner, config.origin);
  const session = (file: string) => checkedRestore(['reconcile', '--db', database, '--bootstrap', file], identity()).sessionFile as string;
  const sessionFile = session(bootstrapFile);
  const proof = JSON.parse(readFileSync(sessionFile, 'utf8'));
  const replaceSession = () => {
    const gate = checkedRestore(['bootstrap', '--db', database]).bootstrapFile;
    return JSON.parse(readFileSync(session(gate), 'utf8'));
  };
  const pair = generateKeyPairSync('ed25519');
  const trust: FenceTrust = { v: 1, endpoint: config.origin, controllerId: 'fixture-controller', targetId: 'restored-target', signingPublicKey: pair.publicKey.export({ format: 'der', type: 'spki' }).toString('base64url') };
  const trustFile = join(dir, 'trust.json'), evidenceFile = join(dir, 'evidence.json'), planFile = join(dir, 'plan.json'), alternateSession = join(dir, 'alternate-session.json');
  const write = (path: string, value: unknown) => writeFileSync(path, JSON.stringify(value ?? null), { mode: 0o600 });
  const options = (actor: HumanActor = owner) => {
    const credentials = identity(actor);
    return { ...credentials, env: { ...credentials.env, MINARBOT_RESTORE_TRUST: trustFile } };
  };
  const run = (args: string[], actor: HumanActor = owner) => restoreCommand(args, options(actor));
  const command = (name: string, pin: unknown, actor = owner, gate = sessionFile, paths: string[] = []) => {
    write(trustFile, pin);
    const { exitCode: code, database: ignored, ...value } = checkedRestore([name, '--db', database, '--session', gate, ...paths], options(actor));
    return value;
  };
  const prepare = (): FenceRequest => {
    const { requestFile } = command('fence-request', trust);
    return JSON.parse(readFileSync(requestFile, 'utf8'));
  };
  const signed = (request: FenceRequest, overrides: Partial<Omit<FenceEvidence, 'signature'>> = {}): FenceEvidence => {
    const unsigned = { request, fenceId: 'fixture-fence-1', processFenced: true as const, routeFenced: true as const, admissionsFenced: true as const, observedAt: Date.now(), expiresAt: Date.now() + 30000, ...overrides };
    return { ...unsigned, signature: sign(null, Buffer.from(canonicalJson(fenceEvidenceBody(unsigned))), pair.privateKey).toString('base64url') };
  };
  const verify = (input: unknown, pin: unknown = trust, actor = owner, gate = sessionFile) => {
    write(evidenceFile, input);
    return command('verify-fence', pin, actor, gate, ['--evidence', evidenceFile]);
  };
  const plan = (input: unknown) => {
    write(evidenceFile, input);
    const result = command('activation-plan', trust, owner, sessionFile, ['--evidence', evidenceFile]);
    return JSON.parse(readFileSync(result.planFile, 'utf8'));
  };
  const activate = (input: unknown, plan: any, pin: unknown = trust, actor = owner, gate = proof) => {
    write(evidenceFile, input);
    write(planFile, plan);
    write(alternateSession, gate);
    return command('activate', pin, actor, alternateSession, ['--evidence', evidenceFile, '--plan', planFile]);
  };
  return { dir, database, owner, proof, trust, trustFile, sessionFile, evidenceFile, prepare, signed, verify, plan, activate, replaceSession, run, db: () => db,
    reopen() { db.close(); db = testDatabase(database); }, close() { db.close(); rmSync(dir, { recursive: true, force: true }); } };
}

test('signed infrastructure fence preflight survives restart and exact retry without activating restore', async () => {
  const f = await fixture();
  try {
    const request = f.prepare(), evidence = f.signed(request), before = f.db().prepare('SELECT * FROM authority_state').get();
    const result = f.verify(evidence);
    assert.deepEqual(result, { state: 'read_only_recovery', fenceVerified: true, requestId: request.requestId, fenceId: 'fixture-fence-1', expiresAt: evidence.expiresAt });
    assert.deepEqual(f.db().prepare('SELECT * FROM authority_state').get(), before);
    f.reopen(); assert.deepEqual(f.verify(evidence), result);
    assert.equal(f.db().prepare('SELECT count(*) n FROM tasks').get()!.n, 0);
    f.db().exec("CREATE TRIGGER reject_fence_request BEFORE UPDATE ON restore_fence_request BEGIN SELECT RAISE(ABORT,'fixture request failure'); END");
    assert.throws(() => f.prepare()); assert.deepEqual(f.verify(evidence), result);
    f.db().exec('DROP TRIGGER reject_fence_request');
    f.prepare(); assert.throws(() => f.verify(evidence), /Infrastructure fence evidence/);
    assert.equal(f.db().prepare('SELECT serving_state FROM authority_state').get()!.serving_state, 'read_only_recovery');
  } finally { f.close(); }
});

test('restore fence rejects forged, stale, mismatched, self-supplied and replaced-session proofs', async () => {
  const f = await fixture();
  try {
    const request = f.prepare(), evidence = f.signed(request), otherId = '33333333-3333-4333-8333-333333333333';
    const wrongKey = generateKeyPairSync('ed25519');
    for (const input of [
      { ...evidence, signature: sign(null, Buffer.from(canonicalJson(fenceEvidenceBody(evidence))), wrongKey.privateKey).toString('base64url') },
      { ...evidence, signature: evidence.signature + '=' },
      { ...evidence, signingPublicKey: f.trust.signingPublicKey },
      { ...evidence, processFenced: false }, { ...evidence, routeFenced: false }, { ...evidence, admissionsFenced: false },
      f.signed({ ...request, endpoint: 'https://other.example' }), f.signed({ ...request, controllerId: 'other' }), f.signed({ ...request, targetId: 'other' }),
      f.signed({ ...request, sourceAuthority: otherId }), f.signed({ ...request, recoveryGeneration: otherId }), f.signed({ ...request, ownerAccount: otherId }), f.signed({ ...request, requestId: otherId }),
      f.signed({ ...request, challenge: randomBytes(32).toString('base64url') }), f.signed(request, { observedAt: request.createdAt - 1 }),
      f.signed(request, { observedAt: Date.now() + 60000 }), f.signed(request, { expiresAt: Date.now() - 1 }), f.signed(request, { expiresAt: Date.now() + 120000 }),
    ]) assert.throws(() => f.verify(input));
    for (const pin of [{ ...f.trust, signingPublicKey: wrongKey.publicKey.export({ format: 'der', type: 'spki' }).toString('base64url') }, { ...f.trust, endpoint: 'https://other.example' }, { ...f.trust, targetId: 'other' }]) assert.throws(() => f.verify(evidence, pin));
    assert.throws(() => f.verify(evidence, null));
    assert.throws(() => f.verify(evidence, f.trust, { ...f.owner, issuer: 'https://other.example' }));
    const replacement = f.replaceSession();
    assert.throws(() => f.verify(evidence));
    const replacementFile = join(f.dir, 'replacement.json');
    writeFileSync(replacementFile, JSON.stringify(replacement), { mode: 0o600 });
    assert.throws(() => f.verify(evidence, f.trust, f.owner, replacementFile));
    assert.equal(f.db().prepare('SELECT serving_state FROM authority_state').get()!.serving_state, 'read_only_recovery');
  } finally { f.close(); }
});

test('operator CLI creates and verifies bounded private fence files while keeping recovery read-only', async () => {
  const f = await fixture();
  const { trustFile, sessionFile, evidenceFile } = f;
  writeFileSync(trustFile, JSON.stringify(f.trust), { mode: 0o600 });
  function run(args: string[], subject = f.owner.subject) {
    const value = f.run(args.slice(1), { ...f.owner, subject });
    return { code: value.exitCode, value, output: JSON.stringify(value) };
  }
  const prepareArgs = ['restore', 'fence-request', '--db', f.database, '--session', sessionFile], verifyArgs = ['restore', 'verify-fence', '--db', f.database, '--session', sessionFile, '--evidence', evidenceFile];
  try {
    const lock = holdRestoreGuard(f.database); try { const busy = await run(prepareArgs); assert.equal(busy.code, 1); assert.match(busy.value.error, /canonical/i); } finally { lock.close(); }
    const prepared = await run(prepareArgs); assert.equal(prepared.code, 0, prepared.output); assert.equal(prepared.value.state, 'read_only_recovery'); assert.equal(statSync(prepared.value.requestFile).mode & 0o777, 0o600); assert(!prepared.output.includes(f.proof.session));
    const request: FenceRequest = JSON.parse(readFileSync(prepared.value.requestFile, 'utf8')); writeFileSync(evidenceFile, JSON.stringify(f.signed(request)), { mode: 0o600 });
    const verified = await run(verifyArgs); assert.equal(verified.code, 0, verified.output); assert.equal(verified.value.fenceVerified, true); assert.deepEqual((await run(verifyArgs)).value, verified.value);
    const symlink = join(f.dir, 'proof-link.json'); symlinkSync(evidenceFile, symlink); assert.equal((await run([...verifyArgs.slice(0, -1), symlink])).code, 1); chmodSync(evidenceFile, 0o644); assert.equal((await run(verifyArgs)).code, 5); chmodSync(evidenceFile, 0o600);
    writeFileSync(evidenceFile, 'x'.repeat(65537)); assert.equal((await run(verifyArgs)).code, 5); writeFileSync(evidenceFile, JSON.stringify({ ...f.signed(request), routeFenced: false })); assert.equal((await run(verifyArgs)).code, 1);
    assert.equal((await run(['restore', 'activate', '--db', f.database])).code, 2); assert.equal(f.db().prepare('SELECT serving_state FROM authority_state').get()!.serving_state, 'read_only_recovery');
    writeFileSync(evidenceFile, JSON.stringify(f.signed(request)));
    const planned = await run(['restore', 'activation-plan', ...verifyArgs.slice(2)]); assert.equal(planned.code, 0, planned.output);
    const planFile = planned.value.planFile, plan = JSON.parse(readFileSync(planFile, 'utf8'));
    assert.equal(statSync(planFile).mode & 0o777, 0o600); assert(!readFileSync(planFile, 'utf8').includes(f.proof.session));
    const activateArgs = ['restore', 'activate', ...verifyArgs.slice(2), '--plan', planFile];
    writeFileSync(planFile, JSON.stringify({ ...plan, recovery: { ...plan.recovery, targetId: 'tampered' } }));
    assert.equal((await run(activateArgs)).code, 5); writeFileSync(planFile, JSON.stringify(plan));
    const held = holdRestoreGuard(f.database); try { assert.equal((await run(activateArgs)).code, 1); } finally { held.close(); }
    const activated = await run(activateArgs); assert.equal(activated.code, 0, activated.output); assert.equal(activated.value.state, 'active');
    assert.deepEqual((await run(activateArgs)).value, activated.value); assert(!activated.output.includes(f.proof.session));
    assert.equal((await run(activateArgs, 'user_other')).code, 1);
  } finally { f.close(); }
});

test('activation plan commits one fenced recovery receipt and exact retry', async () => {
  const f = await fixture();
  try {
    const request = f.prepare(), evidence = f.signed(request), plan = f.plan(evidence);
    assert.equal(plan.state, 'read_only_recovery');
    const result = f.activate(evidence, plan);
    assert.equal(result.state, 'active');
    assert.equal(f.db().prepare("SELECT serving_state FROM authority_state WHERE id=1").get()!.serving_state, 'active');
    assert.equal(f.db().prepare("SELECT scope FROM receipts WHERE request_id=?").get(plan.command.requestId)!.scope, 'restore.activate');
    const cutover = f.db().prepare('SELECT cutover_version,revision FROM authority_state WHERE id=1').get();
    f.reopen();
    assert.equal(canonicalJson(f.activate(evidence, plan)), canonicalJson(result));
    assert.deepEqual(f.db().prepare('SELECT cutover_version,revision FROM authority_state WHERE id=1').get(), cutover);
    await new Promise(resolve => setTimeout(resolve, Math.max(0, evidence.expiresAt - Date.now() + 1)));
    assert.equal(canonicalJson(f.activate(evidence, plan)), canonicalJson(result));
    for (const [actor, session, trust, proof, command] of [
      [f.owner, randomBytes(32).toString('base64url'), f.trust, evidence, plan.command],
      [f.owner, f.proof.session, { ...f.trust, targetId: 'other' }, evidence, plan.command],
      [f.owner, f.proof.session, f.trust, { ...evidence, routeFenced: false }, plan.command],
      [f.owner, f.proof.session, f.trust, evidence, { ...plan.command, scopeDigest: '0'.repeat(64) }],
      [{ ...f.owner, subject: 'other' }, f.proof.session, f.trust, evidence, plan.command],
    ] as const) assert.throws(() => f.activate(proof, { ...plan, command }, trust, actor, { ...f.proof, session }));
    f.db().prepare('UPDATE accounts SET blocked=1 WHERE id=?').run(f.owner.accountId);
    assert.throws(() => f.activate(evidence, plan));
  } finally { f.close(); }
});

test('activation rolls back on receipt failure and rejects changed plans', async () => {
  const f = await fixture();
  try {
    const request = f.prepare(), evidence = f.signed(request), plan = f.plan(evidence);
    assert.throws(() => f.activate(evidence, { ...plan, command: { ...plan.command, scopeDigest: '0'.repeat(64) } }));
    assert.equal(f.db().prepare("SELECT serving_state FROM authority_state WHERE id=1").get()!.serving_state, 'read_only_recovery');
    f.db().exec("CREATE TRIGGER fail_activation_receipt BEFORE INSERT ON receipts BEGIN SELECT RAISE(ABORT,'fixture activation failure'); END");
    assert.throws(() => f.activate(evidence, plan));
    assert.equal(f.db().prepare("SELECT serving_state FROM authority_state WHERE id=1").get()!.serving_state, 'read_only_recovery');
    assert.equal(f.db().prepare("SELECT count(*) AS n FROM receipts WHERE scope='restore.activate'").get()!.n, 0);
    f.db().exec('DROP TRIGGER fail_activation_receipt');
    f.db().exec('UPDATE authority_state SET revision=revision+1 WHERE id=1');
    assert.throws(() => f.activate(evidence, plan), /Recovery evidence changed/);
    const revised = f.plan(evidence);
    f.db().exec("CREATE TABLE restore_activation_proofs(request_id TEXT PRIMARY KEY REFERENCES receipts(request_id),session_hash TEXT NOT NULL,trust_digest TEXT NOT NULL,evidence_json TEXT NOT NULL); CREATE TRIGGER fail_activation_proof BEFORE INSERT ON restore_activation_proofs BEGIN SELECT RAISE(ABORT,'fixture proof failure'); END");
    assert.throws(() => f.activate(evidence, revised));
    assert.equal(f.db().prepare("SELECT serving_state FROM authority_state WHERE id=1").get()!.serving_state, 'read_only_recovery');
    assert.equal(f.db().prepare("SELECT count(*) AS n FROM receipts WHERE scope='restore.activate'").get()!.n, 0);
  } finally { f.close(); }
});

test('activation retains populated revoked grants and held tasks, refusing unsafe recovery state', async () => {
  const member = randomUUID(), installation = randomUUID(), device = randomUUID(), agent = randomUUID(), task = randomUUID();
  const f = await fixture((db, owner, project) => {
    const authority = String(db.prepare('SELECT generation FROM authority_state').get()!.generation);
    db.prepare('INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES (?,?,?,?)').run(member, owner.issuer, 'user_member', Date.now());
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(project, member);
    db.prepare('INSERT INTO installations VALUES (?,?,?,?,1,0,1,?)').run(installation, owner.accountId, 'fixture-key', sha256('installation'), Date.now());
    db.prepare("INSERT INTO devices VALUES (?,?,?,?,?,?,?, ?,1,1,'ready','Asia/Jakarta',1,?,'[]',1)").run(device, installation, owner.accountId, 'signing-key', 'encryption-key', sha256('signing'), sha256('encryption'), JSON.stringify({ authorityGeneration: authority }), Date.now());
    db.prepare('INSERT INTO device_sessions VALUES (?,?,1,?,?,0)').run(sha256('old-session'), device, authority, Date.now() + 60000);
    db.prepare('INSERT INTO agents(id,name,model,instructions) VALUES (?,?,?,?)').run(agent, 'Agent', 'fixture', 'Help');
    db.prepare("INSERT INTO agent_projects VALUES (?,?,'','{\"tools\":[]}',1,0)").run(project, agent);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(project, member, agent);
    db.prepare("INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,'Recover','running',?)").run(task, agent, new Date().toISOString());
    db.prepare("INSERT INTO tasks VALUES (?,?,NULL,?,?,?,?,1,1,?,'{\"tools\":[]}',NULL,NULL,NULL,'running',?,'{}',1,?)").run(task, task, project, agent, owner.accountId, device, sha256('capabilities'), sha256('{}'), Date.now());
    db.prepare("INSERT INTO execution_owners VALUES (?,?,?,1,1,?,'{}',0,?,0)").run(task, device, authority, sha256('executor'), Date.now());
    db.prepare("INSERT INTO routines(id,agent,name,prompt,next_at,enabled) VALUES(?,?,'Fixture','Fixture',0,1)").run(randomUUID(),agent);
  });
  try {
    const evidence = f.signed(f.prepare()), plan = () => f.plan(evidence);
    const initial = plan();
    assert.equal(initial.recovery.revokedMemberships.length, 1); assert.equal(initial.recovery.revokedAssignments.length, 1);
    assert.equal(initial.recovery.revokedDevices.length, 1); assert.equal(initial.recovery.revokedSessions.length, 1);
    assert.equal(initial.recovery.heldTasks[0].state, 'uncertain');
    for (const [unsafe, reset] of [
      ["UPDATE project_members SET deleted=0 WHERE role='member'", "UPDATE project_members SET deleted=1 WHERE role='member'"],
      ['UPDATE collaborator_agents SET enabled=1', 'UPDATE collaborator_agents SET enabled=0'],
      ['UPDATE device_sessions SET revoked=0', 'UPDATE device_sessions SET revoked=1'],
      ["UPDATE devices SET status='ready'", "UPDATE devices SET status='revoked'"],
      ['UPDATE execution_owners SET stopped=0', 'UPDATE execution_owners SET stopped=1'],
      ["UPDATE tasks SET state='queued'", "UPDATE tasks SET state='uncertain'"],
      ['UPDATE turns SET paused=0', 'UPDATE turns SET paused=1'],
      ['UPDATE routines SET enabled=1', 'UPDATE routines SET enabled=0'],
    ]) { f.db().exec(unsafe); assert.throws(plan); assert.throws(() => f.activate(evidence, initial)); f.db().exec(reset); }
    const result = f.activate(evidence, initial);
    assert.equal(result.state, 'active'); assert.equal(f.db().prepare('SELECT state FROM tasks').get()!.state, 'uncertain');
    assert.equal(f.db().prepare('SELECT stopped FROM execution_owners').get()!.stopped, 1); assert.equal(f.db().prepare('SELECT enabled FROM routines').get()!.enabled, 0);
    assert.equal(f.db().prepare("SELECT deleted FROM project_members WHERE role='member'").get()!.deleted, 1);
  } finally { f.close(); }
});

test('activation requires current schema, owner session and fresh infrastructure proof', async () => {
  const f = await fixture();
  try {
    const request = f.prepare(), evidence = f.signed(request);
    const baseline = f.db().prepare('SELECT * FROM schema_migrations WHERE version=6').get()!;
    f.db().prepare('DELETE FROM schema_migrations WHERE version=6').run();
    try { assert.throws(() => f.plan(evidence), /Upgrade the canonical schema/); }
    finally {
      f.db().prepare('INSERT INTO schema_migrations VALUES(?,?,?,?)').run(baseline.version, baseline.name, baseline.checksum, baseline.applied_at);
    }
    const plan = f.plan(evidence);
    const expiry = f.db().prepare('SELECT recovery_expires_at AS expires FROM authority_state').get()!.expires;
    f.db().exec('UPDATE authority_state SET recovery_expires_at=0');
    assert.throws(() => f.verify(evidence), /Recovery evidence/);
    f.db().prepare('UPDATE authority_state SET recovery_expires_at=?').run(expiry);
    for (const input of [f.signed(request, { expiresAt: Date.now() - 1 }), { ...evidence, admissionsFenced: false }, { ...evidence, signature: Buffer.alloc(64).toString('base64url') }]) assert.throws(() => f.plan(input));
    await new Promise(resolve => setTimeout(resolve, Math.max(0, evidence.expiresAt - Date.now() + 1)));
    assert.throws(() => f.activate(evidence, plan));
    const replacement = f.replaceSession();
    assert.throws(() => f.activate(evidence, plan, f.trust, f.owner, replacement));
    assert.equal(f.db().prepare("SELECT serving_state FROM authority_state WHERE id=1").get()!.serving_state, 'read_only_recovery');
  } finally { f.close(); }
});
