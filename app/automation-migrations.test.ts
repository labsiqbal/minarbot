import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { testDatabase } from './test-support/database.ts';
import { startCanonicalApp } from './test-bridge.ts';
import { digestCommand } from './shared/protocol.ts';
import { installCanonicalFixture } from './test-support/canonical-schema.ts';

const key = generateKeyPairSync('rsa', { modulusLength: 2048 });
const issuer = 'https://fixture.clerk.accounts.dev';
const origin = 'http://127.0.0.1';
const now = Date.now();
const due = now - 960000;
const graph = JSON.stringify({ entry: 'out', maxSteps: 1, nodes: [{ id: 'out', kind: 'output' }] });

function fixture() {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-automation-restart-'));
  const database = join(dir, 'state.sqlite'), db = testDatabase(database);
  const owner = randomUUID(), project = randomUUID(), agent = randomUUID(), home = randomUUID(), installation = randomUUID();
  installCanonicalFixture(db);
  db.prepare("INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES (?,?,'user_owner',?)").run(owner, issuer, now);
  db.prepare("INSERT INTO projects(id,account,name,private) VALUES (?,?,'Private',1)").run(project, owner);
  db.prepare("INSERT INTO project_members VALUES (?,?,'owner',1,0)").run(project, owner);
  db.prepare("INSERT INTO agents(id,name,model,instructions) VALUES (?,'Agent','fixture','Help')").run(agent);
  db.prepare("INSERT INTO agent_projects VALUES (?,?,'',?,1,0)").run(project, agent, JSON.stringify({ v: 1, tools: [], resources: [] }));
  db.prepare("INSERT INTO installations VALUES (?,?,'key','thumb',1,0,1,?)").run(installation, owner, now);
  db.prepare(`INSERT INTO devices VALUES (?,?,?,'key','key','sig','enc','{}',1,0,'offline','Asia/Jakarta',1,NULL,'["execution"]',1)`).run(home, installation, owner);
  db.prepare('INSERT INTO agent_authority VALUES (?,?,?,NULL,1)').run(agent, owner, home);
  const generation = String(db.prepare('SELECT generation FROM authority_state WHERE id=1').get()!.generation);
  const scope = (table: string, key: unknown[], kind: string, root: string) => {
    db.prepare("INSERT INTO record_scopes VALUES (?,?,?,?,'owner',?,?)").run(table, JSON.stringify(key), owner, project, kind, root);
  };
  scope('agents', [agent], 'agent', agent);
  let app: Awaited<ReturnType<typeof startCanonicalApp>> | undefined;
  const start = async () => {
    app = await startCanonicalApp({
      database, fixedClock: now, syncKey: () => new Uint8Array(32),
      auth: { secretKey: 'sk_test_fixture', publishableKey: 'pk_test_fixture', issuer, origin, development: true, jwtKey: key.publicKey.export({ type: 'spki', format: 'pem' }).toString() },
    });
  };
  const request = async (path: string, body?: { requestId: string; [key: string]: unknown }, status = 200, subject = 'user_owner') => {
    const header = Buffer.from(JSON.stringify({ alg: 'RS256', kid: 'fixture', typ: 'JWT' })).toString('base64url');
    const payload = Buffer.from(JSON.stringify({ iss: issuer, sub: subject, sid: `sess_${subject}`, azp: app!.origin, nbf: Math.floor(now / 1000) - 1, exp: Math.floor(now / 1000) + 300 })).toString('base64url');
    const input = `${header}.${payload}`;
    const token = `${input}.${sign('RSA-SHA256', Buffer.from(input), key.privateKey).toString('base64url')}`;
    const response = await fetch(app!.origin + path, {
      method: body ? 'POST' : 'GET',
      headers: { 'X-Minarbot-Protocol': '1', 'X-Minarbot-Request-Id': body?.requestId ?? randomUUID(), 'Content-Type': 'application/json', Authorization: `Bearer ${token}`, Origin: app!.origin },
      body: body ? JSON.stringify(body) : undefined,
    });
    const data = await response.json();
    assert.equal(response.status, status, JSON.stringify(data));
    return data.ok ? (body ? data : data.data) : data.error;
  };
  const sync = (cursor?: string, status = 200, subject = 'user_owner') => {
    const query = new URLSearchParams({ projectId: project });
    if (cursor) query.set('cursor', cursor);
    return request('/api/sync?' + query, undefined, status, subject);
  };
  const ready = () => db.prepare("UPDATE devices SET status='ready',last_seen=? WHERE id=?").run(now, home);
  return { db, owner, project, home, agent, generation, scope, start, request, sync, ready,
    path: (id: string) => `/api/projects/${project}/workflows/${id}/schedule`,
    stop: async () => { await app?.close(); app = undefined; },
    close: async () => { await app?.close(); db.close(); rmSync(dir, { recursive: true, force: true }); },
  };
}
type Fixture = ReturnType<typeof fixture>;
async function admission(f: Fixture, path: string, total: number) {
  const deadline = performance.now() + 8000;
  while (performance.now() < deadline) {
    const current = await f.request(path);
    if (current.occurrences.length === total && current.occurrences[0].state === 'admitted') return current;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.fail('Workflow schedule did not admit its overdue occurrence');
}
const foreignKeys = (f: Fixture) => assert.deepEqual(f.db.prepare('PRAGMA foreign_key_check').all(), []);
const command = (revision: number, spec: unknown, enabled = true) => {
  const id = randomUUID();
  return { requestId: id, id, revision, spec, enabled, input: 'Private input' };
};

function saveReceipt(f: Fixture, path: string, input: { requestId: string }, record: string, data: unknown) {
  const payloadDigest = digestCommand(['minarbot-retained-command-v1', f.owner, f.project, 'POST', path, input]);
  const receipt = { requestId: input.requestId, actorId: f.owner, projectId: f.project, payloadDigest, status: 201, recordId: record, recordRevision: 1, committedAt: now };
  f.db.prepare('INSERT INTO receipts VALUES (?,?,?,?,?,201,?,?)')
    .run(input.requestId, f.owner, f.project, `retained:${path}`, payloadDigest, JSON.stringify({ status: 201, receipt, data }), now);
  return receipt;
}

function routine(f: Fixture, interval: number, anchor: number, enabled = false) {
  const id = randomUUID();
  f.db.prepare("INSERT INTO routines(id,agent,name,prompt,next_at,interval_ms,enabled,revision) VALUES (?,?,'Routine','Private prompt',?,?,?,2)")
    .run(id, f.agent, anchor, interval, Number(enabled));
  f.db.prepare("INSERT INTO routine_schedule_bindings VALUES (?,?,?,1,?,?,'Asia/Jakarta',1,?)")
    .run(id, f.project, f.owner, f.generation, f.home, JSON.stringify({ kind: 'interval', amount: interval / 60000, unit: 'minute', anchorAt: anchor }));
  f.scope('routines', [id], 'agent', f.agent);
  return id;
}

test('Rust preserves exact legacy elapsed milliseconds during routine catch-up', async () => {
  const f = fixture();
  try {
    const anchor = now - (2 * 65000 - 1);
    const id = routine(f, 65000, anchor, true);
    await f.start();
    const path = `/api/projects/${f.project}/routines/${id}`;
    const detail = await f.request(path);
    assert.deepEqual(detail.schedule, { kind: 'interval', amount: 65000 / 60000, unit: 'minute', anchorAt: anchor });
    const deadline = performance.now() + 8000;
    while (performance.now() < deadline) {
      const pending = (await f.request(path + '/runs')).items.find((row: { state: string }) => row.state === 'pending');
      if (pending) {
        assert.equal(pending.dueAt, anchor);
        assert.equal(pending.missedCount, 1);
        return;
      }
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.fail('Routine did not record its offline catch-up');
  } finally { await f.close(); }
});

test('Rust retains a stored whole-minute routine anchor on metadata-only edit', async () => {
  const f = fixture();
  try {
    const id = routine(f, 300000, now);
    await f.start();
    const path = `/api/projects/${f.project}/routines/${id}`;
    const { schedule } = await f.request(path);
    assert.deepEqual(schedule, { kind: 'interval', amount: 5, unit: 'minute', anchorAt: now });
    // The scheduler advances next_at independently of the original anchor.
    const advanced = now + 300000;
    f.db.prepare('UPDATE routines SET next_at=? WHERE id=?').run(advanced, id);
    const requestId = randomUUID();
    const edited = await f.request(path + '/edit', {
      requestId, id: requestId, revision: 2, name: 'Renamed', prompt: 'Private prompt', schedule, enabled: false,
    });
    assert.equal(edited.data.next_at, advanced);
    assert.deepEqual(edited.data.schedule, schedule);
  } finally { await f.close(); }
});

test('Workflow observations, history and receipts survive restart', async () => {
  const f = fixture();
  try {
    const workflow = randomUUID(), spec = { kind: 'instant', at: due };
    f.db.prepare("INSERT INTO workflows VALUES (?,?,'Workflow',1,0)").run(workflow, f.agent);
    f.db.prepare('INSERT INTO workflow_versions VALUES (?,1,?)').run(workflow, graph);
    f.scope('workflows', [workflow], 'workflow', workflow);
    f.scope('workflow_versions', [workflow, 1], 'workflow', workflow);
    f.db.prepare("INSERT INTO schedules(id,project,agent,home_device,initiator_account,assignment_revision,authority_generation,kind,zone,zone_revision,spec_json,next_at,enabled,revision) VALUES (?,?,?,?,?,1,?,'instant','Asia/Jakarta',1,?,?,1,1)").run(workflow, f.project, f.agent, f.home, f.owner, f.generation, JSON.stringify(spec), due);
    f.db.prepare("INSERT INTO workflow_schedule_targets VALUES (?,?,'Private input')").run(workflow, workflow);
    f.db.prepare("INSERT INTO occurrences(schedule,occurrence_id,due_at,schedule_revision,zone_revision,initiator_account,assignment_revision,authority_generation,task,state,missed_count,run,admission_id) VALUES (?,'historical',?,1,1,?,1,?,NULL,'pending',0,NULL,?)").run(workflow, due, f.owner, f.generation, randomUUID());
    const input = command(0, spec);
    const receipt = saveReceipt(f, f.path(workflow), input, workflow, { id: workflow, spec, revision: 1, enabled: true });
    const schedules = f.db.prepare('SELECT * FROM schedules').all(), history = f.db.prepare('SELECT * FROM occurrences').all(), receipts = f.db.prepare('SELECT * FROM receipts').all();
    await f.start();
    const observations = (value: { page: { events: { recordKind: string; revision: number }[] } }) =>
      value.page.events.filter(event => event.recordKind.startsWith('workflow_schedule')).map(event => [event.recordKind, event.revision]);
    const snapshot = await f.sync();
    assert.deepEqual(observations(snapshot).sort(), [['workflow_schedule', 1], ['workflow_schedule_occurrence', 1]]);
    for (let restart = 0; restart < 2; restart++) {
      assert.deepEqual(f.db.prepare('SELECT * FROM schedules').all().map(row => ({ ...row })), schedules.map(row => ({ ...row })));
      assert.deepEqual(f.db.prepare('SELECT * FROM occurrences').all().map(row => ({ ...row })), history.map(row => ({ ...row })));
      assert.deepEqual(f.db.prepare('SELECT * FROM receipts').all(), receipts);
      assert(f.db.prepare("SELECT name FROM sqlite_schema WHERE name='workflow_schedule_cron_cursors'").get());
      assert.deepEqual((await f.request(f.path(workflow), input)).receipt, receipt);
      foreignKeys(f);
      if (restart === 0) { await f.stop(); await f.start(); }
    }
    assert.deepEqual((await f.sync(snapshot.nextCursor)).page.events, []);
    f.ready();
    await admission(f, f.path(workflow), 1);
    assert.deepEqual(observations(await f.sync(snapshot.nextCursor)).sort(), [['workflow_schedule', 2], ['workflow_schedule_occurrence', 2]]);
  } finally { await f.close(); }
});
