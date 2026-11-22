import test from 'node:test';
import { createClerkClient } from '@clerk/backend';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { markMinarbotData, testDatabase } from './test-support/database.ts';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { installCanonicalFixture } from './test-support/canonical-schema.ts';
import { checkedRestore, restoreCommand, restoreWorkspace, workspaceSnapshot } from './test-support/backup.ts';
import { createMailWorker } from './test-support/legacy-mail-worker.ts';
import { startApp } from './test-bridge.ts';
import { restoreIdentity } from './test-support/restore-identity.ts';
import { sha256 } from './shared/protocol.ts';
import { canonicalHttp } from './test-support/canonical-http.ts';
import type { HumanActor } from './test-support/auth.ts';

test('canonical backup restores private evidence with fresh authority and no revived invitation or execution', async () => {
  const f = canonicalHttp('minarbot-restore-');
  const { dir, db, owner } = f, destination = join(dir, 'restored.sqlite');
  const member = f.addAccount('user_member');
  const mail={keyId:randomUUID(),key:Buffer.alloc(32,7),lookupKey:Buffer.alloc(32,8),from:'Minarbot <owner@example.com>',origin:'https://minarbot.example'};
  const clerk = createClerkClient({ ...f.auth, telemetry: { disabled: true } });
  clerk.users.getUser = async subject => ({ id: subject, banned: false, locked: false, emailAddresses: [{ emailAddress: 'member@example.com', verification: { status: 'verified' } }] }) as Awaited<ReturnType<typeof clerk.users.getUser>>;
  const options = { clerk, mail: () => mail, mailFrom: mail.from, mailOrigin: mail.origin };
  await f.start(options);
  const project = randomUUID();
  await f.request('/api/projects', 'POST', {requestId:randomUUID(),id:project,name:'Private recovery'});
  db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(project,member.accountId);
  const invitation=randomUUID(),token=Buffer.alloc(32,1).toString('base64url'),authority=String(db.prepare('SELECT generation FROM authority_state').get()!.generation);
  db.prepare("INSERT INTO invitations VALUES (?,?,?,? ,NULL,'sent',?,'[]',?,NULL,1)").run(invitation,project,authority,sha256('member@example.com'),sha256(Buffer.from(token,'base64url')),Date.now()+60000);
  db.prepare("INSERT INTO mail_outbox VALUES (?,1,?,'queued',NULL,NULL,0,NULL,?)").run(invitation,Buffer.from('private token envelope'),sha256('mail'));
  const installation=randomUUID(),device=randomUUID(),agent=randomUUID();
  db.prepare('INSERT INTO installations VALUES (?,?,?,?,1,0,1,?)').run(installation,owner.accountId,'installation-key',sha256('installation'),Date.now());
  db.prepare("INSERT INTO devices VALUES (?,?,?,?,?,?,?,'{}',1,1,'ready','Asia/Jakarta',1,?,'[\"execution\"]',1)").run(device,installation,owner.accountId,'signing-key','encryption-key',sha256('signing'),sha256('encryption'),Date.now());
  db.prepare("INSERT INTO device_sessions VALUES (?,?,1,?,?,0)").run(sha256('old-device-session'),device,authority,Date.now()+60000);
  db.prepare('INSERT INTO agents(id,name,model,instructions) VALUES (?,?,?,?)').run(agent,'Agent','fixture','Help');
  db.prepare("INSERT INTO agent_projects VALUES (?,?,'','{\"tools\":[]}',1,0)").run(project,agent);
  db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(project,member.accountId,agent);
  const task=randomUUID(),snapshot=randomUUID(),request=randomUUID(),action=randomUUID(),payload=JSON.stringify({v:1,kind:'direct',message:{text:'Recover',files:[]}});
  db.prepare("INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,?,'running',?)").run(task,agent,'Recover',new Date().toISOString());
  db.prepare("INSERT INTO tasks VALUES (?,?,NULL,?,?,?,?,1,1,?,'{\"tools\":[]}',NULL,NULL,NULL,'running',?,?,1,?)").run(task,task,project,agent,owner.accountId,device,sha256('capabilities'),sha256(payload),payload,Date.now());
  db.prepare("INSERT INTO receipts VALUES (?,?,?,'fixture',?,200,'{}',?)").run(request,owner.accountId,project,sha256('receipt'),Date.now());
  db.prepare("INSERT INTO task_context_snapshots VALUES (?,1,?,?,1,1,1,1,?,'{}',?,?)").run(task,snapshot,authority,sha256('{}'),request,Date.now());
  db.prepare("INSERT INTO task_actions(id,task,sequence,action_kind,authority_generation,device_generation,executor_generation,intent_digest,context_snapshot_id,context_digest,grant_revision,state,committed_at,effect_status) VALUES (?,?,0,'read',?,1,1,?,?,?,1,'committed',?,'unknown')").run(action,task,authority,sha256('intent'),snapshot,sha256('{}'),Date.now());
  db.prepare("INSERT INTO execution_owners VALUES (?,?,?,1,1,?,'{}',0,?,0)").run(task,device,authority,sha256('executor'),Date.now());
  let backup:Awaited<ReturnType<typeof workspaceSnapshot>>|undefined;
  try{
    const retained={receipts:db.prepare('SELECT * FROM receipts ORDER BY request_id').all(),context:db.prepare('SELECT * FROM task_context_snapshots').all(),history:db.prepare('SELECT id,agent,text,reply,created FROM turns').all(),payload:db.prepare('SELECT id,logical_turn,project,agent,initiator_account,home_device,payload_digest,payload_json FROM tasks').all()};
    backup=await workspaceSnapshot(db);const bytes=readFileSync(backup.file);
    // Revocations newer than this backup are unavailable to its restored copy.
    await f.request(`/api/invitations/${invitation}/revoke`, 'POST', {requestId:randomUUID(),expectedRevision:1});
    restoreWorkspace(backup.file,destination);
    assert.deepEqual(readFileSync(backup.file),bytes);assert.equal(statSync(destination).mode&0o777,0o600);
    const restored=testDatabase(destination);
    try{
      const current=restored.prepare('SELECT * FROM authority_state').get()!;
      assert.notEqual(current.generation,authority);assert.equal(current.serving_state,'read_only_recovery');
      assert.match(String(current.recovery_session_hash), /^[a-f0-9]{64}$/);
      assert.equal(restored.prepare('SELECT revoked FROM device_sessions').get()!.revoked,1);
      assert.equal(restored.prepare('SELECT status FROM devices').get()!.status,'revoked');
      assert.equal(restored.prepare("SELECT deleted FROM project_members WHERE role='member'").get()!.deleted,1);
      assert.equal(restored.prepare("SELECT deleted FROM project_members WHERE role='owner'").get()!.deleted,0);
      assert.equal(restored.prepare('SELECT enabled FROM collaborator_agents').get()!.enabled,0);
      assert.equal(restored.prepare('SELECT state FROM invitations').get()!.state,'revoked');
      assert.equal(restored.prepare('SELECT state FROM mail_outbox').get()!.state,'revoked');
      assert.equal((restored.prepare('SELECT ciphertext FROM mail_outbox').get()!.ciphertext as Uint8Array).length,0);
      assert.equal(restored.prepare('SELECT state FROM task_actions').get()!.state,'uncertain');
      assert.equal(restored.prepare('SELECT state FROM tasks').get()!.state,'uncertain');
      assert.equal(restored.prepare('SELECT paused FROM turns').get()!.paused,1);
      assert.equal(restored.prepare('SELECT stopped FROM execution_owners').get()!.stopped,1);
      assert.equal(restored.prepare('SELECT count(*) n FROM receipts').get()!.n,2);
      assert.deepEqual(restored.prepare('SELECT * FROM receipts ORDER BY request_id').all(),retained.receipts);
      assert.deepEqual(restored.prepare('SELECT * FROM task_context_snapshots').all(),retained.context);
      assert.deepEqual(restored.prepare('SELECT id,agent,text,reply,created FROM turns').all(),retained.history);
      assert.deepEqual(restored.prepare('SELECT id,logical_turn,project,agent,initiator_account,home_device,payload_digest,payload_json FROM tasks').all(),retained.payload);
      assert.deepEqual(restored.prepare('PRAGMA foreign_key_check').all(),[]);
      await f.start({ ...options, database: destination });
      const accept = () => f.request(`/api/invitations/${invitation}/accept`, 'POST', {requestId:randomUUID(),mailboxProof:token,expectedRevision:1}, member);
      await assert.rejects(accept, {code:'authority_unavailable'});
      restored.exec("UPDATE authority_state SET serving_state='active'");
      await assert.rejects(accept, {code:'resource_not_found'});
      restored.exec("UPDATE authority_state SET serving_state='read_only_recovery'");
      await f.stop();
      let deliveries = 0;
      const provider = createServer((_req, res) => {
        deliveries++;
        res.writeHead(200, { 'Content-Type': 'application/json' }).end('{"id":"unexpected-restore-delivery"}');
      });
      await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
      try {
        const address = provider.address();
        assert(address && typeof address !== 'string');
        await f.start({ ...options, database: destination, mailApiKey: 'fixture', mailEndpoint: `http://127.0.0.1:${address.port}/emails` });
        await new Promise(resolve => setTimeout(resolve, 1100));
        assert.equal(deliveries, 0);
        assert.equal(restored.prepare('SELECT state FROM mail_outbox WHERE id=?').get(invitation)!.state, 'revoked');
      } finally {
        await f.stop();
        provider.closeAllConnections();
        await new Promise<void>(resolve => provider.close(() => resolve()));
      }
      const second=join(dir,'second.sqlite'),again=await workspaceSnapshot(restored);
      try{restoreWorkspace(again.file,second);const reloaded=testDatabase(second);try{assert.notEqual(reloaded.prepare('SELECT generation FROM authority_state').get()!.generation,current.generation);}finally{reloaded.close();}}finally{again.cleanup();}
    }finally{restored.close();}
    await assert.rejects(startApp({database:destination}),/canonical/i);
  }finally{backup?.cleanup();await f.close();}
});

test('canonical restore disables retained automation and rejects changed authority or format', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-restore-format-'));
  const source = testDatabase(join(dir, 'source.sqlite'));
  try {
    installCanonicalFixture(source);
    source.exec(`INSERT INTO agents(id,name,model,instructions) VALUES ('agent','Agent','fixture','Help');
      INSERT INTO routines(id,agent,name,prompt,next_at,enabled) VALUES ('routine','agent','Fixture','Help',0,1);
      INSERT INTO workflows(id,agent,name,revision) VALUES ('workflow','agent','Fixture',1);
      INSERT INTO workflow_runs(id,workflow,version,agent,graph,input,last,cursor,status,created)
        VALUES ('run','workflow',1,'agent','{}','','','out','ready','fixture');
      INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES ('account','fixture','owner',0);
      INSERT INTO installations(id,account,signing_public_key,signing_thumbprint,revision,created)
        VALUES ('installation','account','fixture','fixture',1,0);
      INSERT INTO devices VALUES ('device','installation','account','fixture','fixture','sig','enc','{}',1,0,'offline','UTC',1,NULL,'[]',1);
      INSERT INTO turns(id,agent,text,status,created) VALUES ('turn','agent','Wait','running','fixture');
      INSERT INTO turn_waits(id,turn,call_id,call,event,secret_ref,secret_generation,source_device_id,deadline,status,after_output,after_action)
        VALUES ('wait','turn','call','{}','fixture','fixture',1,'device',0,'waiting',0,0);`);
    const backup = await workspaceSnapshot(source);
    try {
      const target = join(dir, 'target.sqlite');
      restoreWorkspace(backup.file, target);
      const restored = testDatabase(target);
      try {
        assert.equal(restored.prepare('SELECT serving_state FROM authority_state').get()?.serving_state, 'read_only_recovery');
        assert.equal(restored.prepare('SELECT enabled FROM routines').get()?.enabled, 0);
        assert.equal(restored.prepare('SELECT status FROM workflow_runs').get()?.status, 'failed');
        assert.equal(restored.prepare('SELECT status FROM turn_waits').get()?.status, 'closed');
      } finally { restored.close(); }

      const tampered = testDatabase(backup.file);
      tampered.exec("UPDATE minarbot_backup_authority SET generation='wrong'");
      const invalid = join(dir, 'invalid.sqlite');
      assert.throws(() => restoreWorkspace(backup.file, invalid), /authority mismatch/);
      assert(!existsSync(invalid));
      tampered.exec("UPDATE minarbot_backup SET format='minarbot-workspace',version=1");
      tampered.close();
      assert.throws(() => restoreWorkspace(backup.file, invalid), /backup format/);
      assert(!existsSync(invalid));
    } finally { backup.cleanup(); }
  } finally { source.close(); rmSync(dir, { recursive: true, force: true }); }
});

test('read-only restore upgrades legacy canonical owner marker before reconcile',async()=>{
  const dir=mkdtempSync(join(tmpdir(),'minarbot-restore-legacy-marker-')),db=testDatabase(join(dir,'source.sqlite'));
  installCanonicalFixture(db);
  const owner={accountId:randomUUID(),issuer:'https://fixture.example',subject:'owner',sessionId:'owner-session',expiresAt:Date.now()+60000};
  db.prepare('INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES (?,?,?,?)').run(owner.accountId,owner.issuer,owner.subject,Date.now());
  const project=randomUUID();db.prepare('INSERT INTO projects(id,account,name,private,deleted,revision) VALUES (?,?,?,1,0,1)').run(project,owner.accountId,'Owner project');
  db.prepare("INSERT INTO project_members(project,account,role,grant_revision,deleted) VALUES (?,?, 'owner',1,0)").run(project,owner.accountId);
  const backup=await workspaceSnapshot(db);db.close();
  try{
    const legacy=testDatabase(backup.file);const generation=legacy.prepare('SELECT generation FROM minarbot_backup_authority').get()!.generation;
    legacy.exec('ALTER TABLE minarbot_backup_authority RENAME TO minarbot_backup_authority_new; CREATE TABLE minarbot_backup_authority(generation TEXT NOT NULL) STRICT;');
    legacy.prepare('INSERT INTO minarbot_backup_authority VALUES (?)').run(generation);legacy.exec('DROP TABLE minarbot_backup_authority_new');legacy.close();
    const restoredPath=join(dir,'restored.sqlite');
    const { bootstrapFile } = checkedRestore(['--read-only', backup.file, '--db', restoredPath]);
    const restored=testDatabase(restoredPath);try{
      assert.equal(restored.prepare('SELECT owner_account FROM minarbot_backup_authority').get()!.owner_account,owner.accountId);
      const { sessionFile } = checkedRestore(['reconcile','--db',restoredPath,'--bootstrap',bootstrapFile], restoreIdentity(owner, 'https://minarbot.example')());
      assert.equal(JSON.parse(readFileSync(sessionFile,'utf8')).recoveryGeneration,JSON.parse(readFileSync(bootstrapFile,'utf8')).recoveryGeneration);
    }finally{restored.close();}
  }finally{backup.cleanup();rmSync(dir,{recursive:true,force:true});}
});

test('operator CLI restores a canonical backup read-only without script credentials',async()=>{
  const dir=mkdtempSync(join(tmpdir(),'minarbot-restore-cli-')),db=testDatabase(join(dir,'source.sqlite'));installCanonicalFixture(db);
  const backup=await workspaceSnapshot(db);db.close();
  try{
    const target=join(dir,'restored.sqlite'),result=checkedRestore(['--read-only',backup.file,'--db',target]);
    assert.equal(result.exitCode,0);assert.equal(result.database,target);assert.equal(result.state,'read_only_recovery');
    const restored=testDatabase(target);try{assert.equal(restored.prepare('SELECT serving_state FROM authority_state').get()!.serving_state,'read_only_recovery');}finally{restored.close();}
    assert.equal(restoreCommand(['--read-only',backup.file,'--db',target,'--activate']).exitCode, 2);
    const legacy=join(dir,'legacy.sqlite'),old=testDatabase(legacy);markMinarbotData(old);old.exec("CREATE TABLE minarbot_backup(format TEXT,version INTEGER); INSERT INTO minarbot_backup VALUES ('minarbot-workspace',1)");old.close();
    const invalid=join(dir,'invalid.sqlite');assert.throws(()=>checkedRestore(['--read-only',legacy,'--db',invalid]),/canonical backup/);assert(!existsSync(invalid));
  }finally{backup.cleanup();rmSync(dir,{recursive:true,force:true});}
});

test('operator CLI refuses a backup without the Minarbot data marker and leaves no target',async()=>{
  const dir=mkdtempSync(join(tmpdir(),'minarbot-restore-unmarked-')),db=testDatabase(join(dir,'source.sqlite'));installCanonicalFixture(db);
  const backup=await workspaceSnapshot(db);db.close();
  try{
    const old=testDatabase(backup.file);old.exec('PRAGMA application_id=0');old.close();
    const target=join(dir,'restored.sqlite');
    assert.throws(()=>checkedRestore(['--read-only',backup.file,'--db',target]),/does not carry the Minarbot data marker/);
    assert(!existsSync(target));
  }finally{backup.cleanup();rmSync(dir,{recursive:true,force:true});}
});

test('local restore bootstrap requires recorded owner identity and issues expiring ingest-only proof',()=>{
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-restore-proof-')), database = join(dir, 'recovery.sqlite');
  const db = testDatabase(database); chmodSync(database, 0o600); installCanonicalFixture(db);
  const owner: HumanActor = { accountId: randomUUID(), issuer: 'https://fixture.example', subject: 'owner', sessionId: 'owner-session', expiresAt: Date.now() + 60_000 };
  const stranger = { ...owner, accountId: randomUUID(), subject: 'stranger' };
  for (const actor of [owner, stranger]) db.prepare('INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES (?,?,?,?)').run(actor.accountId, actor.issuer, actor.subject, Date.now());
  for (const [actor, name] of [[owner, 'Owner only'], [stranger, 'Other project']] as const) {
    const id = randomUUID();
    db.prepare('INSERT INTO projects(id,account,name,private) VALUES (?,?,?,0)').run(id, actor.accountId, name);
    db.prepare("INSERT INTO project_members VALUES (?,?,'owner',1,0)").run(id, actor.accountId);
  }
  db.exec('CREATE TABLE minarbot_backup_authority(generation TEXT,owner_account TEXT)');
  db.prepare('INSERT INTO minarbot_backup_authority SELECT generation,? FROM authority_state').run(owner.accountId);
  const identity = restoreIdentity(owner, 'https://minarbot.example');
  const bootstrap = () => checkedRestore(['bootstrap', '--db', database]).bootstrapFile;
  const reconcile = (path: string, actor: HumanActor = owner) => checkedRestore(['reconcile', '--db', database, '--bootstrap', path], identity(actor));
  try {
    assert.throws(bootstrap, /Recovery evidence/);
    db.exec("UPDATE authority_state SET serving_state='read_only_recovery'");
    const gate = bootstrap();
    const original = JSON.parse(readFileSync(gate,'utf8'));
    const changed = join(dir, 'changed.json');
    writeFileSync(changed, JSON.stringify({ ...original, recoveryGeneration: randomUUID() }), { mode: 0o600 });
    assert.throws(() => reconcile(changed), /recovery authority/);
    assert.throws(() => reconcile(gate, stranger), /Recovery evidence/);
    assert.throws(() => reconcile(gate, { ...owner, issuer: 'https://wrong.example' }), /Session is invalid/);
    const { sessionFile } = reconcile(gate);
    const proof = JSON.parse(readFileSync(sessionFile,'utf8'));
    assert.equal(proof.recoveryGeneration, original.recoveryGeneration);
    assert.equal(statSync(sessionFile).mode & 0o777, 0o600);
    assert.throws(() => reconcile(gate), /Recovery evidence/);
    const state = db.prepare('SELECT * FROM authority_state').get()!;
    assert(!JSON.stringify(state).includes(proof.session)); assert(!JSON.stringify(state).includes(original.bootstrap));
    const expiring = bootstrap();
    db.exec('UPDATE authority_state SET recovery_expires_at=0');
    assert.throws(() => reconcile(expiring), /Recovery evidence/);
    assert.equal(db.prepare('SELECT count(*) n FROM tasks').get()!.n, 0);
    assert.equal(db.prepare('SELECT serving_state FROM authority_state').get()!.serving_state, 'read_only_recovery');
  } finally { db.close(); rmSync(dir, { recursive: true, force: true }); }
});
