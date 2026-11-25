import test from 'node:test';
import assert from 'node:assert/strict';
import { createClerkClient } from '@clerk/backend';
import { createHash, createHmac, createDecipheriv, createPublicKey, diffieHellman, hkdfSync, generateKeyPairSync, sign, randomUUID, randomBytes } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, rmSync, readFileSync, existsSync, statSync, writeFileSync, renameSync, symlinkSync } from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer, request as httpRequest } from 'node:http';
import { tmpdir } from 'node:os';
import { createInterface } from 'node:readline';
import { join } from 'node:path';
import { backup, type DatabaseSync } from 'node:sqlite';
import { markMinarbotData, testDatabase } from './test-support/database.ts';
import { startCanonicalApp } from './test-bridge.ts';
import { startDeviceFetchProxy, rustDeviceFixture, withDeviceProvider, startDeviceFaultProxy, startRustDeviceEnrollment, startRustDeviceConnection, enrollRustDevice, startRustDeviceTask, runRustDeviceTask, crashRustDeviceTask, exportRustDeviceRecovery, type RecoveryExport } from './test-support/rust-device-host.ts';
import type { ActionContext } from './test-support/task-protocol.ts';
import { rustBinary } from './rust-binary.ts';
import { rustServerOutput } from './rust-test-server.ts';
import { canonicalJson, ProtocolError, digestCommand, sha256 } from './shared/protocol.ts';
import { onceCandidate } from './test-support/schedule-time.ts';
import sharp from 'sharp';

function memoryKeys() {
  const values = new Map<string, string>();
  return { values, entry(service: string, name: string) { const key = `${service}/${name}`; return { getPassword: () => values.get(key) ?? null, setPassword: (value: string) => { values.set(key, value); } }; } };
}

function fixtureDatabase(path: string) {
  return testDatabase(path);
}

async function workspaceSnapshot(db: DatabaseSync, owner?: string) {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-canonical-snapshot-'));
  const file = join(directory, 'snapshot.sqlite');
  const cleanup = () => rmSync(directory, { recursive: true, force: true });
  try {
    await backup(db, file);
    chmodSync(file, 0o600);
    const snapshot = testDatabase(file);
    try {
      snapshot.exec(`CREATE TABLE minarbot_backup(format TEXT NOT NULL,version INTEGER NOT NULL,created TEXT NOT NULL) STRICT;
        CREATE TABLE minarbot_backup_authority(generation TEXT NOT NULL,owner_account TEXT REFERENCES accounts(id)) STRICT;`);
      snapshot.prepare("INSERT INTO minarbot_backup VALUES ('minarbot-canonical-v6',6,?)").run(new Date().toISOString());
      snapshot.prepare('INSERT INTO minarbot_backup_authority SELECT generation,coalesce(?,(SELECT min(account) FROM projects HAVING count(DISTINCT account)=1)) FROM authority_state WHERE id=1').run(owner ?? null);
    } finally { snapshot.close(); }
    return { file, cleanup };
  } catch (error) { cleanup(); throw error; }
}

async function protectedWaitFixture() {
  const f = await fixture(), runtime = f.device(join(f.dir, 'event-home.sqlite'));
  try {
    await runtime.enroll(f.jwt()); const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
    const home = (await f.send('/api/devices')).body.data[0], project = randomUUID(), agent = randomUUID();
    assert.equal((await f.send('/api/projects', { requestId: randomUUID(), id: project, name: 'Events' })).status, 201);
    assert.equal((await f.send('/api/agents', { requestId: agent, id: agent, name: 'Waiter', model: 'fixture', backupModel: '', instructions: 'Wait', threadLimit: 2, projectId: project, homeDeviceId: home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: home.revision, providerBinding: null })).status, 201);
    const taskId = randomUUID(), payload = { v: 1, kind: 'direct', message: { text: 'Wait', files: [] } }, expectedRevisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 };
    { const admitted = await f.send('/api/tasks', { requestId: taskId, projectId: project, agentId: agent, homeDeviceId: home.id, payload, expectedRevisions, payloadDigest: digestCommand(['minarbot-task-payload-v1', taskId, project, agent, home.id, payload, expectedRevisions]) }); assert.equal(admitted.status, 202, JSON.stringify(admitted.body)); }
    const owned = await runtime.startTask(taskId), context = await runtime.taskContext(taskId, owned.ownership, owned.revision), actionId = randomUUID();
    await assert.rejects(runtime.executeAction({ taskId, actionId, sequence: 0, actionKind: 'canonical', ownership: owned.ownership, payload: { toolCall: { id: 'wait-ready', function: { name: 'wait_for_event', arguments: JSON.stringify({ event: 'ready', timeoutSeconds: 60 }) } }, expectedRevision: context.snapshot.taskRevision + 1 }, context, grantRevision: 1 }, () => assert.fail('No host effect')), { code: 'device_offline' });
    let waitId = '';
    await until(async () => { const detail = (await f.send(`/api/tasks/${taskId}`)).body.data; waitId = detail.results[0]?.content?.id; return !!waitId; });
    await runtime.recoverPending();
    return { ...f, keys: runtime.keys, runtime, connection, home, project, agent, taskId, waitId, ownership: owned.ownership };
  } catch (error) { await runtime.close(); await f.close(); throw error; }
}

test('journal-backed host dispatches once, retries publication only and refuses lost or late commit grants', { timeout: 20000 }, async () => {
  const f = await fixture(), database = join(f.dir, 'device.sqlite');
  const runtime = f.device(database);
  const originalFetch = globalThis.fetch;
  let calls = 0;
  try {
    await runtime.enroll(f.jwt()); const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
    const home = (await f.send('/api/devices')).body.data[0], project = randomUUID(), agent = randomUUID();
    assert.equal((await f.send('/api/projects', { requestId: randomUUID(), id: project, name: 'Executor' })).status, 201);
    assert.equal((await f.send('/api/agents', { requestId: agent, id: agent, name: 'Agent', model: 'fixture', backupModel: '', instructions: 'Research', threadLimit: 2, projectId: project, homeDeviceId: home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: home.revision, providerBinding: null })).status, 201);
    async function action(loseAcquire=false) {
      const taskId = randomUUID(), payload = { v: 1, kind: 'direct', message: { text: 'Run once', files: [] } }, expectedRevisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 };
      assert.equal((await f.send('/api/tasks', { requestId: taskId, projectId: project, agentId: agent, homeDeviceId: home.id, payload, expectedRevisions, payloadDigest: digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions]) })).status, 202);
      if(loseAcquire) {
        globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/acquire')){await response.body?.cancel();throw new Error('Lost acquire ACK');}return response;};
        await assert.rejects(runtime.startTask(taskId),/Lost acquire ACK/);globalThis.fetch=originalFetch;
      }
      const acquired = await runtime.startTask(taskId);
      const context = await runtime.taskContext(taskId, acquired.ownership, acquired.revision);
      return { taskId, actionId: randomUUID(), sequence: 0, actionKind: 'read' as const, ownership: acquired.ownership, payload: { operation: 'synthetic' }, context, grantRevision: 1 };
    }
    const first = await action(true);
    let dropReport = true;
    globalThis.fetch = async (url, init) => {
      const response = await originalFetch(url, init);
      if (dropReport && String(url).endsWith('/reconcile')) { dropReport = false; await response.body?.cancel(); throw new Error('Lost result ACK'); }
      return response;
    };
    const effect = async () => { calls++; return { text: 'Durable result' }; };
    await assert.rejects(runtime.executeAction(first, effect), /Lost result ACK/);
    assert.equal(calls, 1);
    globalThis.fetch = originalFetch;
    assert.equal((await runtime.executeAction(first, effect)).state, 'acknowledged');
    assert.equal((await runtime.executeAction(first, effect)).state, 'acknowledged');
    assert.equal(calls, 1);
    const db = testDatabase(database);
    try {
      assert.equal(db.prepare('SELECT state FROM device_outbox WHERE action_id=?').get(first.actionId)!.state, 'acknowledged');
      assert.equal(db.prepare('SELECT send_marker FROM device_journal WHERE action_id=?').get(first.actionId)!.send_marker, 1);
    } finally { db.close(); }
    for (const mode of ['lost', 'late'] as const) {
      const input = await action();
      globalThis.fetch = async (url, init) => {
        const response = await originalFetch(url, init);
        if (String(url).endsWith('/commit')) {
          if (mode === 'lost') { await response.body?.cancel(); throw new Error('Lost commit ACK'); }
          await new Promise(resolve => setTimeout(resolve, 2100));
        }
        return response;
      };
      await assert.rejects(runtime.executeAction(input, effect));
      globalThis.fetch = originalFetch;
      await assert.rejects(runtime.executeAction(input, effect), error => error instanceof ProtocolError && error.code === 'recovery_required');
      assert.equal(calls, 1);
    }
    const failed=await action();
    await assert.rejects(runtime.executeAction(failed,()=>{calls++;throw new Error('Provider response unavailable');}),/Provider response unavailable/);
    assert.equal((await runtime.recoverAction(failed.taskId,failed.actionId)).state,'uncertain');
    const detail=(await f.send(`/api/tasks/${failed.taskId}`)).body.data,decisionId=randomUUID();
    assert.equal((await f.send(`/api/tasks/${failed.taskId}/control`,{requestId:decisionId,expectedRevision:detail.revision,operation:'resolve_uncertain',decision:{kind:'retry_with_risk',actionId:failed.actionId,acknowledgement:'effect_may_have_happened',reason:'Synthetic test with explicit owner decision'}})).status,200);
    const resolved=(await f.send(`/api/tasks/${failed.taskId}`)).body.data;
    const context=await runtime.taskContext(failed.taskId,failed.ownership,resolved.revision);
    assert.equal((await runtime.executeAction({...failed,actionId:decisionId,sequence:1,resolutionRequestId:decisionId,context},effect)).state,'acknowledged');
    assert.equal(calls,3);
  } finally { globalThis.fetch = originalFetch; await runtime.close(); await f.close(); }
});

test('SIGKILL recovery proves no send before marker and never repeats marked effects or lost result ACKs', async () => {
  const f = await fixture(), database = join(f.dir, 'device.sqlite'), keyring = join(f.dir, 'synthetic-keys.json');
  const options = { database, keyring, server: f.origin() };
  const calls = new Map<string, number>();
  const provider = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    const { taskId } = JSON.parse(body).snapshot;
    calls.set(taskId, (calls.get(taskId) ?? 0) + 1);
    res.end(JSON.stringify({ text: 'Saved effect' }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  assert(address && typeof address !== 'string');
  const origin = `http://127.0.0.1:${address.port}`;
  try {
    writeFileSync(keyring, '{}', { mode: 0o600 });
    const { deviceId } = await enrollRustDevice(options, f.jwt());
    for (const [checkpoint, expected, marked] of [
      ['provider-not-sent', 'not_sent', false],
      ['provider-prepare-reply', 'not_sent', false],
      ['provider-commit-reply', 'not_sent', false],
      ['provider-in-flight', 'uncertain', true],
      ['provider-saved', 'acknowledged', true],
      ['provider-report-reply', 'acknowledged', true],
    ] as const) {
      const { taskId } = await loopTask(f, { deviceId: () => deviceId });
      await crashRustDeviceTask(options, taskId, origin, checkpoint);
      const db = fixtureDatabase(database);
      try {
        const action = db.prepare('SELECT action_id, send_marker FROM device_journal WHERE task_id=? AND sequence=0').get(taskId)!;
        assert.equal(action.send_marker, Number(marked), checkpoint);
        for (let restart = 0; restart < 2; restart++) {
          await until(async () => (await f.send('/api/devices')).body.data[0].status !== 'ready', deviceReconnectTimeout);
          await f.restart();
          const recovery = startRustDeviceConnection(options);
          try {
            await recovery.ready();
            await until(() => db.prepare("SELECT 1 FROM device_outbox WHERE action_id=? AND state='acknowledged'").get(action.action_id!) !== undefined);
            assert.equal(db.prepare('SELECT state FROM device_journal WHERE action_id=?').get(action.action_id!)!.state, expected, checkpoint);
          } finally { await recovery.stop(); }
          assert.equal(calls.get(taskId) ?? 0, marked || restart > 0 ? 1 : 0, checkpoint);
          const detail = (await f.send(`/api/tasks/${taskId}`)).body.data;
          if (expected === 'acknowledged') assert.deepEqual(detail.results[0].content, { text: 'Saved effect' });
          if (expected === 'uncertain') assert.equal(detail.state, 'uncertain');
          if (!marked && restart === 0) {
            assert.equal(detail.state, 'running');
            assert.equal((await runRustDeviceTask(options, taskId, origin)).state, 'completed');
            const continued = db.prepare('SELECT action_id,state,result_json FROM device_journal WHERE task_id=? AND sequence=1').get(taskId)!;
            assert.notEqual(continued.action_id, action.action_id);
            assert.equal(continued.state, 'acknowledged');
            assert.deepEqual(JSON.parse(String(continued.result_json)), { text: 'Saved effect' });
            assert.equal(calls.get(taskId), 1);
          }
        }
      } finally { db.close(); }
    }
  } finally {
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    await f.close();
  }
});

test('Rust exports prepared, in-flight and reported device journal evidence offline', () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-frozen-journal-'));
  const source = new URL('./test-support/fixtures/device-journal-v3/', import.meta.url);
  try {
    const { identity, keyring: syntheticKeys, samples } = JSON.parse(readFileSync(new URL('synthetic.json', source), 'utf8'));
    const database = join(dir, 'home.sqlite'), keyring = join(dir, 'keys.json'), metadata = database + '.host';
    writeFileSync(database, '', { mode: 0o600 });
    const db = testDatabase(database);
    // The fixture is current-format device data, which carries the data marker.
    try { markMinarbotData(db); db.exec(readFileSync(new URL('device.sql', source), 'utf8')); } finally { db.close(); }
    mkdirSync(metadata, { mode: 0o700 });
    writeFileSync(join(metadata, 'identity.json'), JSON.stringify(identity), { mode: 0o600 });
    writeFileSync(keyring, JSON.stringify(syntheticKeys), { mode: 0o600 });

    const options = { database, keyring, server: identity.server };
    const recoveryGeneration = randomUUID();
    const effects = { not_sent: 'none', uncertain: 'unknown', reported: 'known' } as const;
    for (const { taskId, actionId, recoveryState } of samples as { taskId: string; actionId: string; recoveryState: keyof typeof effects }[]) {
      const requestId = randomUUID(), withContent = recoveryState === 'reported';
      const evidence = exportRustDeviceRecovery(options, taskId, [actionId], requestId, recoveryGeneration, withContent);
      assert.equal(evidence.envelope.entries.length, 1);
      const [entry] = evidence.envelope.entries;
      assert.equal(entry.actionId, actionId);
      assert.equal(entry.taskId, taskId);
      assert.equal(entry.state, recoveryState);
      assert.equal(entry.effectStatus, effects[recoveryState]);
      assert.deepEqual(evidence.resultContent, withContent ? { text: 'Saved effect' } : undefined);
      assert.deepEqual(exportRustDeviceRecovery(options, taskId, [actionId], requestId, recoveryGeneration, withContent), evidence);
    }
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('stalled normal requests cannot starve control reads and disconnected requests release capacity', async () => {
  const f=await fixture(), held: ReturnType<typeof httpRequest>[]=[], answered: (number|undefined)[]=[];let holding=true;
  // Probes can take a slot before a held request arrives; replace any held request the server answers.
  const hold=()=>{
    const request=httpRequest(f.origin()+'/api/projects',{method:'POST',headers:{Authorization:`Bearer ${f.jwt()}`,Origin:f.origin(),'Content-Type':'application/json','Content-Length':'8','X-Minarbot-Protocol':'1','X-Minarbot-Request-Id':randomUUID()}});
    request.on('error',()=>{});request.on('response',response=>{answered.push(response.statusCode);response.resume();if(holding)hold();});request.write('{');held.push(request);
  };
  try {
    for(let i=0;i<64;i++) hold();
    await until(async()=> (await f.send('/api/projects')).status===429).catch(error=>{throw new Error(`${error}; held requests answered: ${JSON.stringify(answered)}`);});
    const control=await f.send('/api/session');assert.equal(control.status,200,JSON.stringify(control.body));
    for (const path of [`/api/tasks/${randomUUID()}`, `/api/tasks/${randomUUID()}/results/${randomUUID()}`]) {
      const receipt = await f.send(path); assert.equal(receipt.status,404,JSON.stringify(receipt.body));
    }
    const logout = await f.send('/api/session/logout',{requestId:randomUUID()},'user_logout');
    assert([200,202,503].includes(logout.status),JSON.stringify(logout));
    const taskControl=await f.send(`/api/tasks/${randomUUID()}/control`,{requestId:randomUUID(),expectedRevision:1,operation:'cancel'});
    assert.equal(taskControl.status,404,JSON.stringify(taskControl.body));
    holding=false;for(const request of held)request.destroy();
    await until(async()=> (await f.send('/api/projects')).status===200);
  } finally { holding=false;for(const request of held)request.destroy();await f.close(); }
});

test('canonical transfer survives lost RPC reply and canonical restart without duplicate successor or caller effects', async () => {
  const f=await fixture();
  const home=f.device(join(f.dir,'source.sqlite'));
  const target=f.device(join(f.dir,'target.sqlite'));
  const originalFetch=globalThis.fetch;let effects=0;
  try {
    await home.enroll(f.jwt());await target.enroll(f.jwt());const connection=home.connect();await until(()=>connection.state().phase==='connected');
    const devices=(await f.send('/api/devices')).body.data,project=randomUUID(),source=randomUUID(),recipient=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Canonical transfer'})).status,201);
    for(const [agentId,deviceId] of [[source,home.deviceId()],[recipient,target.deviceId()]])assert.equal((await f.send('/api/agents',{requestId:agentId,id:agentId,name:agentId,model:'fixture',backupModel:'',instructions:'Continue',threadLimit:2,projectId:project,homeDeviceId:deviceId,expectedProjectRevision:1,expectedHomeDeviceRevision:devices.find((d:{id:string})=>d.id===deviceId).revision,providerBinding:null})).status,201);
    const channel=randomUUID(),seed=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try {
      const ownerId=String(seed.prepare('SELECT account FROM projects WHERE id=?').get(project)!.account);
      seed.prepare('INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)').run(channel,'Handoff','Continue project work',source);
      for(const agent of [source,recipient]) seed.prepare('INSERT INTO channel_members VALUES (?,?)').run(channel,agent);
      seed.prepare("INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)").run(JSON.stringify([channel]),ownerId,project,channel);
    } finally {seed.close();}
    const taskId=randomUUID(),payload={v:1,kind:'channel',channelId:channel,routing:'assigned',message:{text:'Original request',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1,channel:1};
    assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:source,homeDeviceId:home.deviceId(),payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,source,home.deviceId(),payload,expectedRevisions])})).status,202);
    const owned=await home.startTask(taskId),context=await home.taskContext(taskId,owned.ownership,owned.revision),actionId=randomUUID();
    const transferSchema=(context.snapshot.tools as any[]).find((tool:any)=>tool.function.name==='transfer_task') as any;
    assert.deepEqual(transferSchema.function.parameters.properties.recipient.enum,[recipient]);
    const toolCall={id:'transfer',function:{name:'transfer_task',arguments:JSON.stringify({recipient,task:'Continue from saved evidence'})}};
    const action={taskId,actionId,sequence:0,actionKind:'canonical' as const,ownership:owned.ownership,payload:{toolCall,expectedRevision:owned.revision+1},context,grantRevision:1};
    globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith(`/api/tasks/${taskId}/context-mutations`)){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost canonical ACK');}return response;};
    await assert.rejects(home.executeAction(action,()=>{effects++;return{text:'Must not execute'};}),/Lost canonical ACK/);globalThis.fetch=originalFetch;
    const transferred=(await f.send(`/api/tasks/${taskId}`)).body.data,successor=transferred.currentTaskId;assert.equal(transferred.state,'transferred');assert.notEqual(successor,taskId);
    await f.restart();await until(()=>connection.state().phase==='connected');
    await until(()=>home.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
    assert.equal(effects,0);assert.equal((await f.send(`/api/tasks/${taskId}`)).body.data.currentTaskId,successor);
    const next=(await f.send(`/api/tasks/${successor}`)).body.data;assert.equal(next.homeDeviceId,target.deviceId());assert.equal(next.state,'waiting_device');
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{assert.equal(db.prepare('SELECT count(*) n FROM tasks WHERE logical_turn=?').get(taskId)!.n,2);assert.equal(db.prepare('SELECT count(*) n FROM task_results WHERE action=?').get(actionId)!.n,1);}finally{db.close();}
  } finally {globalThis.fetch=originalFetch;await home.close();await target.close();await f.close();}
});

test('owner recovers revoked-device journal through HTTP without device session or repeated effect',async()=>{
  const f=await fixture(),database=join(f.dir,'recover-device.sqlite');
  const runtime=f.device(database);
  const options = runtime.options, { keyring } = options;
  const exportRecovery = (...args: Parameters<typeof exportRustDeviceRecovery> extends [unknown, ...infer Rest] ? Rest : never) =>
    exportRustDeviceRecovery(options, ...args);
  const originalFetch=globalThis.fetch;let calls=0;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),agent=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Recovery'})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Agent',model:'fixture',backupModel:'',instructions:'Recover',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'One effect',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
    assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
    const owned=await runtime.startTask(taskId),context=await runtime.taskContext(taskId,owned.ownership,owned.revision),actionId=randomUUID();
    globalThis.fetch=async(url,init)=>{if(String(url).endsWith('/reconcile'))throw new Error('Report offline');return originalFetch(url,init);};
    await assert.rejects(runtime.executeAction({taskId,actionId,sequence:0,actionKind:'read',ownership:owned.ownership,payload:{operation:'one effect'},context,grantRevision:1},()=>{calls++;return{text:'Original output'};}),/Report offline/);
    globalThis.fetch=originalFetch;
    const current=(await f.send('/api/devices')).body.data[0];assert.equal((await f.send(`/api/devices/${home.id}/revoke`,{requestId:randomUUID(),expectedRevision:current.revision})).status,200);
    await connection.stop(); await runtime.close();
    const start={requestId:randomUUID(),expectedAuthorityGeneration:owned.ownership.authorityGeneration,sourceDeviceId:home.id,sourceDeviceGeneration:owned.ownership.deviceGeneration};
    const session=await f.send(`/api/tasks/${taskId}/recovery-session`,start);assert.equal(session.status,201,JSON.stringify(session.body));
    assert.equal((await f.send(`/api/tasks/${taskId}/recovery-session`,start)).body.data.session,undefined);
    const generation=session.body.data.recoveryGeneration,proof=session.body.data.session;
    const send=async(operation:string,body:RecoveryExport,origin=f.origin(),subject='user_owner')=>{
      const response=await fetch(f.origin()+`/api/tasks/${taskId}/recovery-${operation}`,{method:'POST',headers:{Authorization:`Bearer ${f.jwt(subject)}`,Origin:origin,'Content-Type':'application/json','X-Minarbot-Protocol':'1','X-Minarbot-Request-Id':body.requestId,'X-Minarbot-Recovery':proof,Connection:'close'},body:JSON.stringify(body)});
      return{status:response.status,body:await response.json()};
    };
    const metadata=exportRecovery(taskId,[actionId],randomUUID(),generation);
    assert.equal((await send('import',metadata,'https://evil.example')).status,403);
    assert.equal((await send('import',metadata,f.origin(),'user_other')).status,404);
    const imported=await send('import',metadata);assert.equal(imported.status,200,JSON.stringify(imported.body));assert.equal(imported.body.data.state,'result_missing');
    await f.restart();
    const result=exportRecovery(taskId,[actionId],randomUUID(),generation,true);
    assert.deepEqual(result.envelope.entries,metadata.envelope.entries);
    const published=await send('results',result);assert.equal(published.status,200,JSON.stringify(published.body));assert.equal((await send('results',result)).status,200);
    assert.deepEqual((await f.send(`/api/tasks/${taskId}`)).body.data.results[0].content,{text:'Original output'});
    assert.equal((await f.send('/api/devices')).body.data[0].status,'revoked');assert.equal(calls,1);
    const rotated=await enrollRustDevice(options,f.jwt(),'rotate');assert(rotated.generation>owned.ownership.deviceGeneration);
    const historical=exportRecovery(taskId,[actionId],randomUUID(),generation,true);
    assert.deepEqual(historical.envelope.entries,metadata.envelope.entries);
    assert.equal((await send('results',historical)).status,200);
    const local=testDatabase(database);let signingRef:string;
    try{signingRef=String(local.prepare('SELECT signing_ref FROM device_enrollment_keys WHERE device_generation=?').get(owned.ownership.deviceGeneration)!.signing_ref);}finally{local.close();}
    const keyName=[...runtime.keys.values.keys()].find(name=>name.endsWith('/'+signingRef))!,privateKey=runtime.keys.values.get(keyName)!;
    const rotatedKeys = JSON.parse(readFileSync(keyring, 'utf8'));
    assert.equal(rotatedKeys[keyName], privateKey);
    delete rotatedKeys[keyName];
    writeFileSync(keyring, JSON.stringify(rotatedKeys), { mode: 0o600 });
    assert.throws(()=>exportRecovery(taskId,[actionId],randomUUID(),generation,true),error=>error instanceof ProtocolError&&error.code==='device_key_unavailable');

    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{assert.equal(db.prepare('SELECT count(*) n FROM task_results').get()!.n,1);assert.equal(db.prepare('SELECT count(*) n FROM recovery_imports').get()!.n,1);assert(!JSON.stringify(db.prepare('SELECT * FROM receipts').all()).includes(proof));}finally{db.close();}
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

test('disaster recovery imports original host result without original server and never enables dispatch',async()=>{
  const f=await fixture(),database=join(f.dir,'home.sqlite'),runtime=f.device(database);
  const options = runtime.options;
  const exportRecovery = (...args: Parameters<typeof exportRustDeviceRecovery> extends [unknown, ...infer Rest] ? Rest : never) =>
    exportRustDeviceRecovery(options, ...args);
  const originalFetch=globalThis.fetch;let calls=0,backup:Awaited<ReturnType<typeof workspaceSnapshot>>|undefined;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),agent=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Disaster recovery'})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Agent',model:'fixture',backupModel:'',instructions:'Recover',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'Original action',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
    assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
    const owned=await runtime.startTask(taskId),context=await runtime.taskContext(taskId,owned.ownership,owned.revision),actionId=randomUUID();
    globalThis.fetch=async(url,init)=>{if(String(url).endsWith('/reconcile'))throw new Error('Original server lost');return originalFetch(url,init);};
    await assert.rejects(runtime.executeAction({taskId,actionId,sequence:0,actionKind:'read',ownership:owned.ownership,payload:{operation:'one call'},context,grantRevision:1},()=>{calls++;return{text:'Saved on original host'};}),/Original server lost/);
    await connection.stop();
    await assert.rejects(runtime.startTask(taskId),error=>error instanceof ProtocolError&&error.code==='device_offline');
    await runtime.close();globalThis.fetch=originalFetch;
    const source=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{backup=await workspaceSnapshot(source);}finally{source.close();}
    const proof=await f.restoreFrom(backup.file);
    assert.notEqual(proof.recoveryGeneration,owned.ownership.authorityGeneration);
    const send=async(kind:string,body:RecoveryExport,session=proof.session,subject='user_owner')=>{
      const response=await fetch(f.origin()+`/api/tasks/${taskId}/recovery-${kind}`,{method:'POST',headers:{Authorization:`Bearer ${f.jwt(subject)}`,Origin:f.origin(),Connection:'close','Content-Type':'application/json','X-Minarbot-Protocol':'1','X-Minarbot-Request-Id':body.requestId,'X-Minarbot-Recovery':session},body:JSON.stringify(body)});
      return{status:response.status,body:await response.json()};
    };
    const metadata=exportRecovery(taskId,[actionId],randomUUID(),proof.recoveryGeneration);
    assert.equal((await send('import',metadata,Buffer.alloc(32).toString('base64url'))).status,403);
    assert.equal((await send('import',metadata,proof.session,'user_other')).status,403);
    const imported=await send('import',metadata);assert.equal(imported.status,200,JSON.stringify(imported.body));assert.equal(imported.body.data.state,'result_missing');
    await f.restart();const content=exportRecovery(taskId,[actionId],randomUUID(),proof.recoveryGeneration,true);
    const published=await send('results',content);assert.equal(published.status,200,JSON.stringify(published.body));assert.equal((await send('results',content)).status,200);
    assert.equal((await f.send(`/api/tasks/${taskId}`)).status,503);
    const restored=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try{
      assert.equal(restored.prepare('SELECT serving_state FROM authority_state').get()!.serving_state,'read_only_recovery');
      assert.deepEqual(JSON.parse(String(restored.prepare('SELECT content_json FROM task_results WHERE action=?').get(actionId)!.content_json)),{text:'Saved on original host'});
      assert.equal(restored.prepare('SELECT count(*) n FROM task_results').get()!.n,1);
      assert.equal(restored.prepare('SELECT stopped FROM execution_owners').get()!.stopped,1);
      assert.equal(restored.prepare('SELECT status FROM devices').get()!.status,'revoked');
      restored.exec('UPDATE authority_state SET recovery_expires_at=0');
      assert.equal((await send('results',content)).status,403);
    }finally{restored.close();}
    assert.equal(calls,1);
  }finally{globalThis.fetch=originalFetch;backup?.cleanup();await runtime.close();await f.close();}
});

test('disaster recovery quarantines signed post-backup actions and tasks without publishing or admitting them',async()=>{
  const f=await fixture(),database=join(f.dir,'home.sqlite'),runtime=f.device(database);
  const options = runtime.options;
  const exportRecovery = (...args: Parameters<typeof exportRustDeviceRecovery> extends [unknown, ...infer Rest] ? Rest : never) =>
    exportRustDeviceRecovery(options, ...args);
  const originalFetch=globalThis.fetch;let calls=0,backup:Awaited<ReturnType<typeof workspaceSnapshot>>|undefined;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),agent=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Quarantine'})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Agent',model:'fixture',backupModel:'',instructions:'Recover',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    const admit=async()=>{
      const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'Post-backup action',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
      assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
      return taskId;
    };
    const knownTask=await admit(),source=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{backup=await workspaceSnapshot(source);}finally{source.close();}
    const unknownTask=await admit(),actions=[];
    globalThis.fetch=async(url,init)=>{if(String(url).endsWith('/reconcile'))throw new Error('Original server lost');return originalFetch(url,init);};
    for(const taskId of [knownTask,unknownTask]){
      const owned=await runtime.startTask(taskId),context=await runtime.taskContext(taskId,owned.ownership,owned.revision),actionId=randomUUID();actions.push({taskId,actionId});
      await assert.rejects(runtime.executeAction({taskId,actionId,sequence:0,actionKind:'read',ownership:owned.ownership,payload:{operation:'one call'},context,grantRevision:1},()=>{calls++;return{text:'Keep result on original host'};}),/Original server lost/);
    }
    await connection.stop(); await runtime.close();globalThis.fetch=originalFetch;const proof=await f.restoreFrom(backup.file);
    const send=async(taskId:string,body:RecoveryExport,session=proof.session,subject='user_owner')=>{
      const response=await fetch(f.origin()+`/api/tasks/${taskId}/recovery-import`,{method:'POST',headers:{Authorization:`Bearer ${f.jwt(subject)}`,Origin:f.origin(),Connection:'close','Content-Type':'application/json','X-Minarbot-Protocol':'1','X-Minarbot-Request-Id':body.requestId,'X-Minarbot-Recovery':session},body:JSON.stringify(body)});
      return{status:response.status,body:await response.json()};
    };
    for(const {taskId,actionId} of actions){
      const metadata=exportRecovery(taskId,[actionId],randomUUID(),proof.recoveryGeneration);
      assert.equal((await send(taskId,metadata,Buffer.alloc(32).toString('base64url'))).status,403);
      assert.equal((await send(taskId,metadata,proof.session,'user_other')).status,403);
      const altered=structuredClone(metadata);altered.signature=Buffer.alloc(64).toString('base64url');assert.equal((await send(taskId,altered)).status,403);
      const result=await send(taskId,metadata);assert.equal(result.status,200,JSON.stringify(result.body));
      assert.deepEqual(result.body.data.quarantinedActionIds,[actionId]);assert.deepEqual(result.body.data.importedActionIds,[]);assert.equal(result.body.data.dispatchable,false);
      assert.equal((await send(taskId,metadata)).body.receipt.requestId,result.body.receipt.requestId);
    }
    await f.restart();
    const restored=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{
      assert.equal(restored.prepare('SELECT count(*) n FROM tasks').get()!.n,1);
      assert.equal(restored.prepare('SELECT count(*) n FROM task_actions').get()!.n,0);
      assert.equal(restored.prepare('SELECT count(*) n FROM task_results').get()!.n,0);
      assert.equal(restored.prepare('SELECT count(*) n FROM recovery_imports').get()!.n,2);
      const privateReceipt=restored.prepare('SELECT project,result_json FROM receipts WHERE scope=?').get(`tasks:recovery-import:${unknownTask}`)!;
      assert.equal(privateReceipt.project,null);assert.equal(JSON.parse(String(privateReceipt.result_json)).receipt.projectId,undefined);
      assert.equal(restored.prepare('SELECT serving_state FROM authority_state').get()!.serving_state,'read_only_recovery');
    }finally{restored.close();}
    assert.equal(calls,2);assert.equal((await f.send(`/api/tasks/${unknownTask}`)).status,404);
  }finally{globalThis.fetch=originalFetch;backup?.cleanup();await runtime.close();await f.close();}
});

test('canonical memory RPC isolates projects and retains committed memory after a lost reply',async()=>{
  const f=await fixture(),runtime=f.device(join(f.dir,'home.sqlite'));
  const originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),other=randomUUID(),agent=randomUUID();
    for(const id of [project,other])assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id,name:id})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Agent',model:'fixture',backupModel:'',instructions:'Remember',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    assert.equal((await f.send(`/api/projects/${other}/agents/${agent}`,{requestId:randomUUID(),expectedProjectRevision:1,expectedAgentRevision:1,expectedAuthorityRevision:1,expectedParticipationRevision:0,rules:'',capabilities:{v:1,tools:['remember_memory','search_memory','select_memory'],resources:[]}},'user_owner','PUT')).status,201);
    const start=async(projectId:string)=>{
      const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'Remember here',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
      assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,projectId,agent,home.id,payload,expectedRevisions])})).status,202);
      return {...await runtime.startTask(taskId),sequence:0};
    };
    const first=await start(project);
    const run=async(task:typeof first,name:string,args:unknown,lose=false)=>{
      const detail=(await f.send(`/api/tasks/${task.taskId}`)).body.data,context=await runtime.taskContext(task.taskId,task.ownership,detail.revision),actionId=randomUUID();
      const input={taskId:task.taskId,actionId,sequence:task.sequence++,actionKind:'canonical' as const,ownership:task.ownership,payload:{toolCall:{id:actionId,function:{name,arguments:JSON.stringify(args)}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
      if(lose){
        globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost memory result');}return response;};
        await assert.rejects(runtime.executeAction(input,()=>assert.fail('Host effect')),/Lost memory result/);globalThis.fetch=originalFetch;
        await f.restart();await until(()=>connection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
      }else await runtime.executeAction(input,()=>assert.fail('Host effect'));
      return (await f.send(`/api/tasks/${task.taskId}`)).body.data.results.find((row:{actionId:string})=>row.actionId===actionId).content;
    };
    const saved=await run(first,'remember_memory',{text:'Project A private memory'},true);assert.equal(saved.revision,1);
    assert.equal((await run(first,'select_memory',{items:[{id:saved.id,revision:1,essential:true}]})).items[0].revision,2);
    assert.equal((await run(first,'search_memory',{})).items[0].text,'Project A private memory');
    assert.equal((await run(first,'remember_memory',{text:'x'.repeat(2001)})).status,400);
    assert.equal((await run(first,'search_memory',{})).items.length,1);
    const second=await start(other);assert.deepEqual((await run(second,'search_memory',{})).items,[]);
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{
      assert.equal(db.prepare('SELECT count(*) n FROM memories').get()!.n,1);assert.equal(db.prepare("SELECT project FROM record_scopes WHERE table_name='memories'").get()!.project,project);
      const scope=db.prepare('SELECT scope FROM memories').get()!.scope;
      db.prepare('INSERT INTO memories(scope,id,text,essential,revision,removed,selection_locked) VALUES (?,?,?,0,1,0,1)').run(scope,randomUUID(),'Unmapped legacy private content');
    }finally{db.close();}
    await assert.rejects(run(first,'search_memory',{}),error=>error instanceof ProtocolError&&error.code==='record_scope_missing');
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

test('canonical history RPC pages scoped originals and retains exact reads after a lost reply',async()=>{
  const f=await fixture(),runtime=f.device(join(f.dir,'home.sqlite'));
  const originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),other=randomUUID(),agent=randomUUID(),thread=randomUUID(),channel=randomUUID();
    for(const id of [project,other])assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id,name:id})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Reader',model:'fixture',backupModel:'',instructions:'Read originals',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    await f.send('/api/session',undefined,'user_foreign');
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    const ownerId=String(db.prepare("SELECT id FROM accounts WHERE clerk_sub='user_owner'").get()!.id),foreignId=String(db.prepare("SELECT id FROM accounts WHERE clerk_sub='user_foreign'").get()!.id);
    const seed=(text:string,rootKind='agent',root=agent,visibility='owner',account=ownerId,projectId=project,mapped=true)=>{
      const id=randomUUID(),row=db.prepare("INSERT INTO turns(id,agent,text,reply,status,created) VALUES (?,?,?,?,'done','fixture')").run(id,agent,text,'Saved reply');
      if(mapped)db.prepare("INSERT INTO record_scopes VALUES ('turns',?,?,?,?,?,?)").run(JSON.stringify([id]),account,projectId,visibility,rootKind,root);
      return {id,seq:Number(row.lastInsertRowid)};
    };
    db.prepare('INSERT INTO threads(id,agent,name) VALUES (?,?,?)').run(thread,agent,'Side');
    db.prepare("INSERT INTO record_scopes VALUES ('threads',?,?,?,'owner','thread',?)").run(JSON.stringify([thread]),ownerId,project,thread);
    db.prepare("INSERT INTO channels(id,name,instructions,lead) VALUES (?,'Room','Read',?)").run(channel,agent);
    db.prepare('INSERT INTO channel_members VALUES (?,?)').run(channel,agent);
    db.prepare("INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)").run(JSON.stringify([channel]),ownerId,project,channel);
    const oldest=seed('x'.repeat(9000)+'ORIGINAL_END');
    const direct=Array.from({length:6},(_,i)=>seed(`Direct ${i}`));
    const side=seed('SIDE_ONLY','thread',thread),shared=seed('CHANNEL_SHARED','channel',channel,'project',foreignId);
    const hidden=[seed('PRIVATE_HUMAN','agent',agent,'owner',foreignId),seed('FOREIGN_PROJECT','agent',agent,'owner',ownerId,other),seed('UNMAPPED','agent',agent,'owner',ownerId,project,false)];
    db.close();
    const start=async(kind:'direct'|'side'|'channel')=>{
      const taskId=randomUUID(),payload={v:1,kind,...(kind==='side'?{threadId:thread}:kind==='channel'?{channelId:channel,routing:'assigned'}:{}),message:{text:'Read history',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1,...(kind==='side'?{thread:1}:kind==='channel'?{channel:1}:{})};
      assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
      return {...await runtime.startTask(taskId),sequence:0};
    };
    const first=await start('direct');
    const futureDb=fixtureDatabase(join(f.dir,'canonical.sqlite'));const future=randomUUID();
    futureDb.prepare("INSERT INTO turns(id,agent,text,reply,status,created) VALUES (?,?,'FUTURE','Future reply','done','fixture')").run(future,agent);
    futureDb.prepare("INSERT INTO record_scopes VALUES ('turns',?,?,?,'owner','agent',?)").run(JSON.stringify([future]),ownerId,project,agent);futureDb.close();
    const run=async(task:typeof first,name:string,args:unknown,lose=false)=>{
      const detail=(await f.send(`/api/tasks/${task.taskId}`)).body.data,context=await runtime.taskContext(task.taskId,task.ownership,detail.revision),actionId=randomUUID();
      const input={taskId:task.taskId,actionId,sequence:task.sequence++,actionKind:'canonical' as const,ownership:task.ownership,payload:{toolCall:{id:actionId,function:{name,arguments:JSON.stringify(args)}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
      if(lose){
        globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost history result');}return response;};
        await assert.rejects(runtime.executeAction(input,()=>assert.fail('Host effect')),/Lost history result/);globalThis.fetch=originalFetch;
        const changed=fixtureDatabase(join(f.dir,'canonical.sqlite'));changed.prepare('UPDATE turns SET text=?,revision=revision+1 WHERE id=?').run('Changed after read',oldest.id);changed.close();
        await f.restart();await until(()=>connection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
      }else await runtime.executeAction(input,()=>assert.fail('Host effect'));
      return (await f.send(`/api/tasks/${task.taskId}`)).body.data.results.find((row:{actionId:string})=>row.actionId===actionId).content;
    };
    const page=await run(first,'read_history',{});assert.deepEqual(page.messages.map((row:{id:string})=>row.id),direct.slice(1).reverse().map(row=>row.id));assert.equal(page.next_before,direct[1].seq);
    const next=await run(first,'read_history',{before:page.next_before});assert.deepEqual(next.messages.map((row:{id:string})=>row.id),[direct[0].id,oldest.id]);assert.equal(next.next_before,null);assert.equal(next.messages[1].truncated,true);
    const original=await run(first,'read_message',{id:oldest.id},true);assert.equal(original.text,'x'.repeat(8000));assert.equal(original.next_offset,8000);assert.deepEqual(original.attachments,[]);assert.deepEqual(original.generatedFiles,[]);assert.deepEqual(original.questions,[]);
    const restored=fixtureDatabase(join(f.dir,'canonical.sqlite'));restored.prepare('UPDATE turns SET text=?,revision=revision+1 WHERE id=?').run('x'.repeat(9000)+'ORIGINAL_END',oldest.id);restored.close();
    const tail=await run(first,'read_message',{id:oldest.id,offset:8000});assert.equal(tail.text,'x'.repeat(1000)+'ORIGINAL_END');assert.equal(tail.next_offset,null);
    assert.match((await run(first,'read_message',{id:hidden[0].id})).error,/not found/);
    assert.equal((await run(first,'read_history',{before:0})).status,400);assert.equal((await run(first,'read_message',{id:oldest.id,offset:-1})).status,400);
    for(const [kind,id] of [['side',side.id],['channel',shared.id]] as const){const task=await start(kind),result=await run(task,'read_history',{});assert.deepEqual(result.messages.map((row:{id:string})=>row.id),[id]);}
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

test('canonical skill reads recover exact pinned text after lost response, disable and server restart', async () => {
  const f=await fixture(),runtime=f.device(join(f.dir,'home.sqlite'));
  const originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),agent=randomUUID(),skill=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Skill reads'})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Reader',model:'fixture',backupModel:'',instructions:'Read installed skills',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite')),owner=String(db.prepare("SELECT id FROM accounts WHERE clerk_sub='user_owner'").get()!.id);
    try{
      const versions = [
        { name: 'pinned-reader', description: 'Read immutable support files', content: 'PINNED_CONTENT' },
        { name: 'latest-reader', description: 'Uninstalled version', content: 'LATEST_PRIVATE' },
      ];
      db.prepare('INSERT INTO skills VALUES (?,?,2)').run(skill,versions[1].name);
      for (const [index, version] of versions.entries()) {
        const { name, description, content } = version;
        db.prepare('INSERT INTO skill_versions VALUES (?,?,?,?,?)').run(skill,index+1,name,description,content);
        const files: { path: string; content: string; encoding?: 'base64' }[] = [{ path: 'SKILL.md', content: `---\nname: ${name}\ndescription: ${description}\n---\n${content}` }];
        if (index === 0) files.push(
          { path: 'notes.txt', content: 'x'.repeat(8000)+'ORIGINAL_TAIL' },
          { path: 'asset.bin', content: Buffer.from('BINARY_PRIVATE').toString('base64'), encoding: 'base64' },
        );
        db.prepare('INSERT INTO skill_files VALUES (?,?,?)').run(skill,index+1,JSON.stringify(files));
      }
      db.prepare('INSERT INTO agent_skills(agent,skill,version,enabled,revision,removed) VALUES (?,?,1,1,1,0)').run(agent,skill);
      for(const [table,key] of [['skills',[skill]],['skill_versions',[skill,1]],['agent_skills',[agent,skill]],['skill_files',[skill,1]]] as const)db.prepare("INSERT INTO record_scopes VALUES (?,?,?,?,'owner','agent',?)").run(table,JSON.stringify(key),owner,project,agent);
    }finally{db.close();}
    const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'Inspect pinned skill',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
    assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
    const owned=await runtime.startTask(taskId);let sequence=0;
    const run=async(name:string,args:unknown,lose=false)=>{
      const detail=(await f.send(`/api/tasks/${taskId}`)).body.data,context=await runtime.taskContext(taskId,owned.ownership,detail.revision),actionId=randomUUID();
      const input={taskId,actionId,sequence:sequence++,actionKind:'canonical' as const,ownership:owned.ownership,payload:{toolCall:{id:actionId,function:{name,arguments:JSON.stringify(args)}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
      if(lose){
        globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost skill result');}return response;};
        await assert.rejects(runtime.executeAction(input,()=>assert.fail('Host effect')),/Lost skill result/);globalThis.fetch=originalFetch;
        const changed=fixtureDatabase(join(f.dir,'canonical.sqlite'));changed.prepare('UPDATE agent_skills SET enabled=0,revision=revision+1 WHERE agent=? AND skill=?').run(agent,skill);changed.close();
        await f.restart();await until(()=>connection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
      }else await runtime.executeAction(input,()=>assert.fail('Host effect'));
      return (await f.send(`/api/tasks/${taskId}`)).body.data.results.find((row:{actionId:string})=>row.actionId===actionId).content;
    };
    assert.deepEqual((await run('list_skills',{})).items,[{id:skill,version:1,name:'pinned-reader',description:'Read immutable support files'}]);
    const read=await run('read_skill',{id:skill});assert.equal(read.content,'PINNED_CONTENT');assert.equal(read.version,1);assert(!JSON.stringify(read).includes('LATEST_PRIVATE'));assert(!JSON.stringify(read).includes('BINARY_PRIVATE'));
    const file=await run('read_skill_file',{id:skill,path:'notes.txt',offset:8000},true);assert.equal(file.content,'ORIGINAL_TAIL');assert.equal(file.next_offset,null);
    assert.match((await run('read_skill',{id:skill})).error,/not found/);
    assert.equal((await f.send(`/api/tasks/${taskId}`)).body.data.results.length,4);
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

test('canonical agent directory isolates projects and recovers exact roster after lost reply', async () => {
  const f=await fixture(),runtime=f.device(join(f.dir,'home.sqlite')),originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),other=randomUUID(),agent=randomUUID(),peer=randomUUID(),foreign=randomUUID();
    for(const id of [project,other])assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id,name:'Directory'})).status,201);
    for(const [id,projectId] of [[agent,project],[peer,project],[foreign,other]])assert.equal((await f.send('/api/agents',{requestId:id,id,name:id===peer?'Original teammate':'Agent',model:'fixture',backupModel:'',instructions:'Project teammate',threadLimit:2,projectId,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'List teammates',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
    assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
    const owned=await runtime.startTask(taskId);let context=await runtime.taskContext(taskId,owned.ownership,owned.revision);const actionId=randomUUID(),cursorAction=randomUUID();
    assert((context.snapshot.tools as {function:{name:string}}[]).some(tool=>tool.function.name==='list_agents'));
    await runtime.executeAction({taskId,actionId:cursorAction,sequence:0,actionKind:'canonical',ownership:owned.ownership,payload:{toolCall:{id:cursorAction,function:{name:'list_agents',arguments:JSON.stringify({after:peer.toUpperCase()})}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1},()=>assert.fail('Host effect'));
    assert.deepEqual((await f.send(`/api/tasks/${taskId}`)).body.data.results.find((row:{actionId:string})=>row.actionId===cursorAction).content,{agents:[],next_after:null});
    context=await runtime.taskContext(taskId,owned.ownership,(await f.send(`/api/tasks/${taskId}`)).body.data.revision);
    const input={taskId,actionId,sequence:1,actionKind:'canonical' as const,ownership:owned.ownership,payload:{toolCall:{id:actionId,function:{name:'list_agents',arguments:'{}'}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
    globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost directory result');}return response;};
    await assert.rejects(runtime.executeAction(input,()=>assert.fail('Host effect')),/Lost directory result/);globalThis.fetch=originalFetch;
    const changed=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{changed.prepare("UPDATE agents SET name='Later teammate',revision=revision+1 WHERE id=?").run(peer);changed.prepare('UPDATE agent_projects SET deleted=1,revision=revision+1 WHERE project=? AND agent=?').run(project,peer);}finally{changed.close();}
    await f.restart();await until(()=>connection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
    const detail=(await f.send(`/api/tasks/${taskId}`)).body.data;
    assert.equal(detail.results.length,2);assert.deepEqual(detail.results[1].content,{agents:[{id:peer,name:'Original teammate',role:'',description:'Project teammate',status:'idle',queuedMessages:0}],next_after:null});
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{assert.equal(db.prepare('SELECT count(*) n FROM task_results WHERE action=?').get(actionId)!.n,1);assert.equal(db.prepare('SELECT count(*) n FROM task_reports WHERE action=?').get(actionId)!.n,1);}finally{db.close();}
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

test('canonical teammate sends retain one scoped delivery on its fixed home after lost reply and restart', async () => {
  const f=await fixture(),runtime=f.device(join(f.dir,'sender.sqlite')),remote=f.device(join(f.dir,'recipient.sqlite')),originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());await remote.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const homes=(await f.send('/api/devices')).body.data,home=homes.find((row:{id:string})=>row.id===runtime.deviceId()),otherHome=homes.find((row:{id:string})=>row.id===remote.deviceId()),project=randomUUID(),agent=randomUUID(),peer=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Messages'})).status,201);
    for(const [id,device] of [[agent,home],[peer,otherHome]])assert.equal((await f.send('/api/agents',{requestId:id,id,name:id===agent?'Sender':'Recipient',model:'fixture',backupModel:'',instructions:'Project teammate',threadLimit:2,projectId:project,homeDeviceId:device.id,expectedProjectRevision:1,expectedHomeDeviceRevision:device.revision,providerBinding:null})).status,201);
    const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'PRIVATE_SENDER_HISTORY',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
    assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
    const owned=await runtime.startTask(taskId),context=await runtime.taskContext(taskId,owned.ownership,owned.revision),actionId=randomUUID();
    const input={taskId,actionId,sequence:0,actionKind:'canonical' as const,ownership:owned.ownership,payload:{toolCall:{id:actionId,function:{name:'send_message',arguments:JSON.stringify({recipients:[peer],text:'Only this text',expects_reply:false})}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
    globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost message result');}return response;};
    await assert.rejects(runtime.executeAction(input,()=>assert.fail('Host effect')),/Lost message result/);globalThis.fetch=originalFetch;
    await f.restart();await until(()=>connection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
    const detail=(await f.send(`/api/tasks/${taskId}`)).body.data,result=detail.results[0].content;
    assert.equal(result.status,'sent');assert.equal(result.expects_reply,false);
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));let childId:string;
    try{
      const children=db.prepare('SELECT turn FROM agent_deliveries WHERE request=?').all(result.request);assert.equal(children.length,1);childId=String(children[0].turn);
      assert.equal(db.prepare('SELECT count(*) n FROM tasks').get()!.n,2);assert.equal(db.prepare('SELECT count(*) n FROM task_results WHERE action=?').get(actionId)!.n,1);
    }finally{db.close();}
    const child=(await f.send(`/api/tasks/${childId}`)).body.data;assert.equal(child.homeDeviceId,otherHome.id);assert.equal(child.state,'waiting_device');
    assert.equal((await f.send(`/api/tasks/${childId}`,undefined,'user_foreign')).status,404);
    const remoteConnection=remote.connect();await until(()=>remoteConnection.state().phase==='connected');const childOwned=await remote.startTask(childId),childContext=await remote.taskContext(childId,childOwned.ownership,childOwned.revision);
    const childConversation=childContext.snapshot.conversation as {message:{text:string;files:string[]};history:unknown[]};assert.deepEqual(childConversation.message,{text:'Only this text',files:[]});assert.deepEqual(childConversation.history,[]);assert(!JSON.stringify(childContext.snapshot).includes('PRIVATE_SENDER_HISTORY'));
    assert(!(childContext.snapshot.tools as {function:{name:string}}[]).some(tool=>['send_message','list_agents','interrupt_agent'].includes(tool.function.name)));
    const paused=await f.send(`/api/tasks/${taskId}/control`,{requestId:randomUUID(),expectedRevision:detail.revision,operation:'pause'});assert.equal(paused.status,200);
    await assert.rejects(remote.taskContext(childId,childOwned.ownership,childOwned.revision),(error:unknown)=>error instanceof ProtocolError&&error.code==='task_not_runnable');
    assert.equal((await f.send(`/api/tasks/${childId}/control`,{requestId:randomUUID(),expectedRevision:childOwned.revision,operation:'pause'})).status,200);
    const resumed=await f.send(`/api/tasks/${taskId}/control`,{requestId:randomUUID(),expectedRevision:paused.body.data.revision,operation:'resume'});assert.equal(resumed.status,200);
    assert.equal((await f.send(`/api/tasks/${childId}`)).body.data.state,'paused');
    const childResumed=await f.send(`/api/tasks/${childId}/control`,{requestId:randomUUID(),expectedRevision:childOwned.revision+1,operation:'resume'});assert.equal(childResumed.status,200);
    await remote.taskContext(childId,childOwned.ownership,childResumed.body.data.revision);
    const continued=await runtime.taskContext(taskId,owned.ownership,resumed.body.data.revision);assert((continued.snapshot.coordination as {request?:string}[]).some(row=>row.request===result.request));
  }finally{globalThis.fetch=originalFetch;await runtime.close();await remote.close();await f.close();}
});

test('canonical setup creation retains one restricted task and creation receipt after lost response and restart',async()=>{
  const f=await fixture(),runtime=f.device(join(f.dir,'setup-home.sqlite')),originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),agent=randomUUID(),binding=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Setup'})).status,201);
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try{db.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','synthetic-setup','Synthetic','ready',1)").run(binding,home.id,db.prepare('SELECT account FROM devices WHERE id=?').get(home.id)!.account);}finally{db.close();}
    const command={requestId:agent,id:agent,name:'New agent',model:'fixture',backupModel:'',instructions:'Build a careful research profile.',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:{bindingId:binding,expectedRevision:1},setup:true};
    globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/api/agents')){assert.equal(response.status,201,await response.clone().text());await response.body?.cancel();throw new Error('Lost setup creation');}return response;};
    await assert.rejects(f.send('/api/agents',command),/Lost setup creation/);globalThis.fetch=originalFetch;
    await f.restart();const retry=await f.send('/api/agents',command);assert.equal(retry.status,200,JSON.stringify(retry.body));
    const lookup=await f.send(`/api/tasks/${agent}`),detail=lookup.body.data;assert.equal(detail.state,'waiting_device');assert.equal(lookup.body.receipt.requestId,agent);assert.equal(lookup.body.receipt.payloadDigest,retry.body.receipt.payloadDigest);
    const connection=runtime.connect();await until(()=>connection.state().phase==='connected');const owned=await runtime.startTask(agent),context=await runtime.taskContext(agent,owned.ownership,owned.revision);
    const tools=context.snapshot.tools as {function:{name:string;parameters?:{required:string[]}}}[];assert.deepEqual(tools.map(row=>row.function.name).sort(),['ask_user','edit_profile']);assert.deepEqual(tools.find(row=>row.function.name==='edit_profile')!.function.parameters!.required,['revision','name','instructions']);
    assert.match(JSON.stringify(context.snapshot.conversation),/Do not start the actual work/);assert.match(JSON.stringify(context.snapshot.conversation),/Build a careful research profile/);
    let providerCalls=0;const providerId=randomUUID();await runtime.executeAction({taskId:agent,actionId:providerId,sequence:0,actionKind:'provider',ownership:owned.ownership,payload:{prompt:'Synthetic setup request'},context,grantRevision:1,bindingId:binding,bindingRevision:1},async()=>{providerCalls++;return {name:'Research partner',instructions:'Compare sources and cite evidence.'};});
    const current=(await f.send(`/api/tasks/${agent}`)).body.data,next=await runtime.taskContext(agent,owned.ownership,current.revision),editId=randomUUID();
    await runtime.executeAction({taskId:agent,actionId:editId,sequence:1,actionKind:'canonical',ownership:owned.ownership,payload:{toolCall:{id:editId,function:{name:'edit_profile',arguments:JSON.stringify({revision:1,name:'Research partner',instructions:'Compare sources and cite evidence.'})}},expectedRevision:next.snapshot.taskRevision+1},context:next,grantRevision:1},()=>assert.fail('Host effect'));
    assert.equal((await f.send('/api/agents',command)).status,200);assert.equal(providerCalls,1);
    const saved=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try{assert.equal(saved.prepare('SELECT count(*) n FROM tasks WHERE id=?').get(agent)!.n,1);assert.equal(saved.prepare('SELECT count(*) n FROM turns WHERE id=?').get(agent)!.n,1);assert.equal(saved.prepare('SELECT count(*) n FROM receipts WHERE request_id=?').get(agent)!.n,1);assert.equal(saved.prepare('SELECT name FROM agents WHERE id=?').get(agent)!.name,'Research partner');}finally{saved.close();}
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

test('canonical caller interruption retains one notice and fences remote deliveries after lost result and restart',async()=>{
  const f=await fixture(),runtime=f.device(join(f.dir,'caller.sqlite')),remote=f.device(join(f.dir,'worker.sqlite')),originalFetch=globalThis.fetch;
  let releaseEffect:(()=>void)|undefined;
  try{
    await runtime.enroll(f.jwt());await remote.enroll(f.jwt());const connection=runtime.connect(),remoteConnection=remote.connect();await until(()=>connection.state().phase==='connected'&&remoteConnection.state().phase==='connected');
    const homes=(await f.send('/api/devices')).body.data,home=homes.find((row:{id:string})=>row.id===runtime.deviceId()),otherHome=homes.find((row:{id:string})=>row.id===remote.deviceId()),project=randomUUID(),agent=randomUUID(),peer=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Interruption'})).status,201);
    for(const [id,device] of [[agent,home],[peer,otherHome]])assert.equal((await f.send('/api/agents',{requestId:id,id,name:id,model:'fixture',backupModel:'',instructions:'Project teammate',threadLimit:2,projectId:project,homeDeviceId:device.id,expectedProjectRevision:1,expectedHomeDeviceRevision:device.revision,providerBinding:null})).status,201);
    const admit=async()=>{
      const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'Caller command',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
      assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
      return runtime.startTask(taskId);
    };
    const input=async(task:Awaited<ReturnType<typeof admit>>,name:string,args:unknown)=>{
      const detail=(await f.send(`/api/tasks/${task.taskId}`)).body.data,context=await runtime.taskContext(task.taskId,task.ownership,detail.revision),actionId=randomUUID();
      return {taskId:task.taskId,actionId,sequence:0,actionKind:'canonical' as const,ownership:task.ownership,payload:{toolCall:{id:actionId,function:{name,arguments:JSON.stringify(args)}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
    };
    const parent=await admit();await runtime.executeAction(await input(parent,'send_message',{recipients:[peer],text:'Work that should stop',expects_reply:true}),()=>assert.fail('Host effect'));
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));let childId:string;
    try{childId=String(db.prepare('SELECT d.turn FROM agent_deliveries d JOIN agent_requests r ON r.id=d.request WHERE r.turn=?').get(parent.taskId)!.turn);}finally{db.close();}
    const child=await remote.startTask(childId),childContext=await remote.taskContext(childId,child.ownership,child.revision),effectId=randomUUID();let effectStarted=false;
    const hold=new Promise<void>(resolve=>{releaseEffect=resolve;});
    const pendingEffect=remote.executeAction({taskId:childId,actionId:effectId,sequence:0,actionKind:'read',ownership:child.ownership,payload:{operation:'synthetic read'},context:childContext,grantRevision:1},async()=>{effectStarted=true;await hold;return {text:'Accepted effect completed after interruption'};});
    await until(()=>effectStarted);const stopper=await admit(),stop=await input(stopper,'interrupt_agent',{recipient:peer,reason:'Changed direction'});
    globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost interruption result');}return response;};
    await assert.rejects(runtime.executeAction(stop,()=>assert.fail('Host effect')),/Lost interruption result/);globalThis.fetch=originalFetch;
    assert.equal((await f.send(`/api/tasks/${childId}`)).body.data.state,'cancelled');
    releaseEffect!();await pendingEffect;
    const cancelled=(await f.send(`/api/tasks/${childId}`)).body.data;assert.equal(cancelled.state,'cancelled');assert.deepEqual(cancelled.results[0].content,{text:'Accepted effect completed after interruption'});
    await f.restart();await until(()=>connection.state().phase==='connected'&&remoteConnection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===stop.actionId&&row.state==='acknowledged'),15000);
    const detail=(await f.send(`/api/tasks/${stopper.taskId}`)).body.data,result=detail.results[0].content;assert.equal(result.status,'interrupted');assert.deepEqual(result.cancelled,[childId]);
    await assert.rejects(remote.taskContext(childId,child.ownership,child.revision),(error:unknown)=>error instanceof ProtocolError&&error.code==='ownership_conflict');
    const saved=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try{const notices=saved.prepare('SELECT t.text,r.expects_reply FROM agent_requests r JOIN agent_deliveries d ON d.request=r.id JOIN turns t ON t.id=d.turn WHERE r.turn=?').all(stopper.taskId);assert.equal(notices.length,1);assert.equal(notices[0].expects_reply,0);assert.match(String(notices[0].text),/Changed direction/);assert.equal(saved.prepare('SELECT count(*) n FROM task_results WHERE action=?').get(stop.actionId)!.n,1);}finally{saved.close();}
    const parentDetail=(await f.send(`/api/tasks/${parent.taskId}`)).body.data;await runtime.taskContext(parent.taskId,parent.ownership,parentDetail.revision);
  }finally{releaseEffect?.();globalThis.fetch=originalFetch;await runtime.close();await remote.close();await f.close();}
});

test('canonical profile edits recover exact result without replacing later edits or current task instructions', async () => {
  const f=await fixture(),runtime=f.device(join(f.dir,'home.sqlite')),originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),agent=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Profile edits'})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Reader',model:'fixture',backupModel:'',instructions:'Original instructions',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    const admit=async(agentRevision:number)=>{
      const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'Edit profile',files:[]}},expectedRevisions={project:1,agent:agentRevision,homeBinding:1,membership:1,assignment:1,capability:1};
      assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);return taskId;
    };
    const taskId=await admit(1),owned=await runtime.startTask(taskId),context=await runtime.taskContext(taskId,owned.ownership,owned.revision),actionId=randomUUID();
    const input={taskId,actionId,sequence:0,actionKind:'canonical' as const,ownership:owned.ownership,payload:{toolCall:{id:actionId,function:{name:'edit_profile',arguments:JSON.stringify({revision:1,name:'Edited',instructions:'Saved instructions'})}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
    globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost profile edit result');}return response;};
    await assert.rejects(runtime.executeAction(input,()=>assert.fail('Host effect')),/Lost profile edit result/);globalThis.fetch=originalFetch;
    const changed=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{changed.prepare("UPDATE agents SET name='Later edit',instructions='Later instructions',revision=revision+1 WHERE id=?").run(agent);}finally{changed.close();}
    await f.restart();await until(()=>connection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
    const detail=(await f.send(`/api/tasks/${taskId}`)).body.data;assert.equal(detail.results.length,1);assert.deepEqual(detail.results[0].content,{name:'Edited',role:'',instructions:'Saved instructions',revision:2});
    const continued=await runtime.taskContext(taskId,owned.ownership,detail.revision),continuedSources=continued.snapshot.sourceRevisions as {kind:string;revision:number}[];assert.deepEqual(continued.snapshot.profile,context.snapshot.profile);assert.equal(continuedSources.find(row=>row.kind==='agent')!.revision,3);
    const nextId=await admit(3),next=await runtime.startTask(nextId),fresh=await runtime.taskContext(nextId,next.ownership,next.revision),freshProfile=fresh.snapshot.profile as {instructions:string;name:string;revision:number};assert.equal(freshProfile.instructions,'Later instructions');assert.equal(freshProfile.name,'Later edit');assert.equal(freshProfile.revision,3);
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

test('canonical provider settings use home binding metadata and recover saved result after lost reply', async () => {
  const f=await fixture(),runtime=f.device(join(f.dir,'home.sqlite')),originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),agent=randomUUID(),binding=randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Provider reads'})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Reader',model:'fixture',backupModel:'backup',instructions:'Read settings',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try{
      const owner=String(db.prepare("SELECT id FROM accounts WHERE clerk_sub='user_owner'").get()!.id);
      db.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','bound-account','Owner','ready',1)").run(binding,home.id,owner);
      db.prepare('UPDATE agent_authority SET provider_binding=? WHERE agent=?').run(binding,agent);
      db.prepare("UPDATE agents SET provider_account='LEGACY_MUST_NOT_LEAK' WHERE id=?").run(agent);
    }finally{db.close();}
    const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'Read provider settings',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
    assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
    const owned=await runtime.startTask(taskId),detail=(await f.send(`/api/tasks/${taskId}`)).body.data,context=await runtime.taskContext(taskId,owned.ownership,detail.revision),actionId=randomUUID();
    const input={taskId,actionId,sequence:0,actionKind:'canonical' as const,ownership:owned.ownership,payload:{toolCall:{id:actionId,function:{name:'read_provider_settings',arguments:'{}'}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
    globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost provider settings result');}return response;};
    await assert.rejects(runtime.executeAction(input,()=>assert.fail('Host effect')),/Lost provider settings result/);globalThis.fetch=originalFetch;
    const changed=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try{changed.prepare("UPDATE provider_bindings SET provider_account_id='Changed',readiness='revoked',revision=2 WHERE id=?").run(binding);}finally{changed.close();}
    await f.restart();await until(()=>connection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
    const results=(await f.send(`/api/tasks/${taskId}`)).body.data.results;
    assert.equal(results.length,1);assert.deepEqual(results[0].content,{revision:1,providerAccount:'bound-account',model:'fixture',backupModel:'backup',configured:true});
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

test('canonical routine reads and edits isolate project configuration and recover original flow after lost reply', async () => {
  const f=await fixture(),runtime=f.device(join(f.dir,'home.sqlite')),originalFetch=globalThis.fetch;
  try{
    await runtime.enroll(f.jwt());const connection=runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data[0],project=randomUUID(),other=randomUUID(),agent=randomUUID(),routine=randomUUID(),hidden=randomUUID();
    for(const id of [project,other])assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id,name:id})).status,201);
    assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Routine reader',model:'fixture',backupModel:'',instructions:'Read project routines',threadLimit:2,projectId:project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    const graph={root:agent,nodes:[agent],links:[]},db=fixtureDatabase(join(f.dir,'canonical.sqlite')),owner=String(db.prepare("SELECT id FROM accounts WHERE clerk_sub='user_owner'").get()!.id);
    try{
      db.exec('ALTER TABLE routines ADD COLUMN secret TEXT');
      for(const [id,projectId] of [[routine,project],[hidden,other]]){
        db.prepare("INSERT INTO routines(id,agent,name,prompt,next_at,interval_ms,enabled,revision,trigger,secret) VALUES (?,?,'Routine','Original prompt',123456,NULL,1,1,'webhook','WEBHOOK_SECRET')").run(id,agent);
        db.prepare("INSERT INTO record_scopes VALUES ('routines',?,?,?,'owner','agent',?)").run(JSON.stringify([id]),owner,projectId,agent);
      }
      db.prepare('INSERT INTO routine_flows VALUES (?,?,1)').run(routine,JSON.stringify(graph));db.prepare("INSERT INTO record_scopes VALUES ('routine_flows',?,?,?,'owner','agent',?)").run(JSON.stringify([routine]),owner,project,agent);
    }finally{db.close();}
    const taskId=randomUUID(),payload={v:1,kind:'direct',message:{text:'Read routine',files:[]}},expectedRevisions={project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
    assert.equal((await f.send('/api/tasks',{requestId:taskId,projectId:project,agentId:agent,homeDeviceId:home.id,payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,agent,home.id,payload,expectedRevisions])})).status,202);
    const owned=await runtime.startTask(taskId);let sequence=0;
    const run=async(name:string,args:unknown,lose=false)=>{
      const detail=(await f.send(`/api/tasks/${taskId}`)).body.data,context=await runtime.taskContext(taskId,owned.ownership,detail.revision),actionId=randomUUID();
      const input={taskId,actionId,sequence:sequence++,actionKind:'canonical' as const,ownership:owned.ownership,payload:{toolCall:{id:actionId,function:{name,arguments:JSON.stringify(args)}},expectedRevision:context.snapshot.taskRevision+1},context,grantRevision:1};
      if(lose){
        globalThis.fetch=async(url,init)=>{const response=await originalFetch(url,init);if(String(url).endsWith('/context-mutations')){assert.equal(response.status,200,await response.clone().text());await response.body?.cancel();throw new Error('Lost routine result');}return response;};
        await assert.rejects(runtime.executeAction(input,()=>assert.fail('Host effect')),/Lost routine result/);globalThis.fetch=originalFetch;
        const changed=fixtureDatabase(join(f.dir,'canonical.sqlite'));changed.prepare("UPDATE routines SET prompt='Changed prompt',revision=revision+1 WHERE id=?").run(routine);changed.prepare("UPDATE routine_flows SET graph='{}',revision=revision+1 WHERE routine=?").run(routine);changed.close();
        await f.restart();await until(()=>connection.state().phase==='connected');await until(()=>runtime.recoveryStatus().results.some(row=>row.actionId===actionId&&row.state==='acknowledged'),15000);
      }else await runtime.executeAction(input,()=>assert.fail('Host effect'));
      return (await f.send(`/api/tasks/${taskId}`)).body.data.results.find((row:{actionId:string})=>row.actionId===actionId).content;
    };
    const list=await run('list_routines',{});assert.deepEqual(list.routines.map((row:{id:string})=>row.id),[routine]);assert.equal(list.next_after,null);assert(!JSON.stringify(list).includes('WEBHOOK_SECRET'));
    const read=await run('read_routine',{id:routine},true);assert.equal(read.prompt,'Original prompt');assert.deepEqual(read.flow,{graph,revision:1});assert(!JSON.stringify(read).includes('WEBHOOK_SECRET'));
    assert.match((await run('read_routine',{id:hidden})).error,/not found/);
    const edit=await run('set_routine_flow',{id:routine,revision:2,links:[]},true);assert.deepEqual(edit,{graph,revision:3});
    const final=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{assert.equal(final.prepare('SELECT revision FROM routine_flows WHERE routine=?').get(routine)!.revision,4);assert.equal(final.prepare('SELECT count(*) n FROM task_results WHERE task=?').get(taskId)!.n,4);}finally{final.close();}
  }finally{globalThis.fetch=originalFetch;await runtime.close();await f.close();}
});

async function routineWebhookFixture() {
  const f = await workflowConfigurationFixture(), routine = randomUUID(), path = `/api/projects/${f.project}/routines/${routine}`;
  const configuration = { requestId: routine, id: routine, revision: 0, name: 'Webhook', prompt: 'PRIVATE_WEBHOOK_PROMPT', nextAt: Date.now() + 600000, intervalSeconds: 300, enabled: true, trigger: 'webhook' };
  try {
    assert.equal((await f.send(`/api/projects/${f.project}/agents/${f.agent}/routines`, configuration)).status, 201);
    const create = { requestId: randomUUID(), revision: 0, rotate: true, filters: [{ pointer: '/count', equals: 4 }] };
    const hook = await f.send(path + '/webhook', create); assert.equal(hook.status, 201, JSON.stringify(hook.body));
    const event = async (secret = hook.body.data.secret, eventId = 'fixture.event', body: string | Uint8Array = '{"event":"ready","payload":{"count":4}}', time = String(Math.floor(Date.now() / 1000)), extra: Record<string,string | undefined> = {}, route = path + '/webhook-events') => {
      const headers = new Headers({ Connection: 'close', 'Content-Type': 'application/json', 'X-Minarbot-Time': time, 'X-Minarbot-Id': eventId, 'X-Minarbot-Signature': createHmac('sha256', secret).update(`${time}\n${eventId}\n`).update(body).digest('hex') });
      for (const [name, value] of Object.entries(extra)) { if (value === undefined) headers.delete(name); else headers.set(name, value); }
      const response = await fetch(f.origin() + route, { method: 'POST', headers, body: typeof body === 'string' ? body : new Uint8Array(body) });
      return { status: response.status, body: await response.json() };
    };
    return { ...f, routine, path, configuration, create, hook: hook.body, event };
  } catch (error) { await f.close(); throw error; }
}

test('canonical routine webhook admits once offline and recovers lost response restart edits and rotation', async () => {
  const f = await routineWebhookFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite')), originalFetch = globalThis.fetch;
  const other = f.device(join(f.dir, 'webhook-other.sqlite'));
  let calls = 0;
  try {
    const secret = f.hook.data.secret, binding = randomUUID(), owner = (await f.send('/api/session')).body.data.accountId;
    assert.equal(f.webhookKeys.values.size, 1); assert([...f.webhookKeys.values.keys()][0].includes('/routine-webhooks/'));
    assert.equal(secret.length, 43); assert.deepEqual((await f.send(f.path + '/webhook', f.create)).body, f.hook);
    db.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','webhook-fixture','Fixture','ready',1)").run(binding, f.home.id, owner);
    db.prepare('UPDATE agent_authority SET provider_binding=?,revision=revision+1 WHERE agent=?').run(binding, f.agent);
    await other.enroll(f.jwt()); const otherConnection = other.connect(); await until(() => otherConnection.state().phase === 'connected');
    const before = (await f.send(f.path)).body.data, syncPath = '/api/sync?projectId=' + f.project, cursor = (await f.send(syncPath)).body.data.nextCursor;
    globalThis.fetch = async (url, init) => {
      const response = await originalFetch(url, init);
      if (String(url).endsWith(f.path + '/webhook-events')) { assert.equal(response.status, 202, await response.clone().text()); await response.body?.cancel(); throw new Error('Lost webhook response'); }
      return response;
    };
    await assert.rejects(f.event(), /Lost webhook response/); globalThis.fetch = originalFetch;
    const replay = await f.event(); assert.equal(replay.status, 200, JSON.stringify(replay.body));
    const accepted = replay.body, task = accepted.data.taskId;
    assert.equal(accepted.data.outcome, 'queued'); assert.equal(accepted.data.source, 'webhook'); assert.equal(accepted.data.homeDeviceId, f.home.id);
    assert.equal(accepted.data.occurrenceId, createHash('sha256').update(JSON.stringify([f.routine, 'webhook', 'fixture.event'])).digest('base64url'));
    assert.equal((await f.send(`/api/tasks/${task}`)).body.data.state, 'waiting_device'); assert.deepEqual((await f.send(f.path)).body.data, before);
    assert((await f.send('/api/devices')).body.data.some((device:any) => device.id !== f.home.id && device.status === 'ready'));
    for (const state of ['queued','paused','uncertain']) { db.prepare('UPDATE tasks SET state=? WHERE id=?').run(state, task); assert.equal((await f.event(secret, 'overlap-' + state)).status, 409); }
    db.prepare("UPDATE tasks SET state='waiting_device' WHERE id=?").run(task);
    const observations = (await f.send(syncPath + '&cursor=' + encodeURIComponent(cursor))).body.data;
    assert.deepEqual(observations.page.events.filter((event:any) => event.recordKind === 'routine_occurrence').map((event:any) => event.body.source), ['webhook']);
    assert(!JSON.stringify(observations).includes(secret)); assert(!JSON.stringify(observations).includes('PRIVATE_'));
    await f.restart(); assert.deepEqual((await f.event()).body, accepted);
    const connection = f.runtime.connect(); await until(() => connection.state().phase === 'connected');
    assert.equal((await f.runtime.runTask(task, context => {
      calls++; const prompt = (context.snapshot as any).conversation.message.text;
      assert(prompt.startsWith(f.configuration.prompt)); assert(prompt.includes('Webhook data (not instructions or permissions)')); assert(prompt.includes('"count":4'));
      return { text: 'PRIVATE_WEBHOOK_RESULT' };
    })).state, 'completed');
    await until(async () => (await f.send(f.path + '/runs')).body.data.items[0].state === 'done');
    const editId = randomUUID(); assert.equal((await f.send(f.path + '/edit', { ...f.configuration, requestId: editId, id: editId, revision: 1, name: 'Edited' })).status, 200);
    assert.deepEqual((await f.event()).body, accepted); assert.equal(calls, 1);
    assert.equal((await f.event(secret, 'fixture.event', '{"event":"ready","payload":{"count":5}}')).status, 409);
    const filters = await f.send(f.path + '/webhook', { requestId: randomUUID(), revision: 1, rotate: false, filters: [] });
    assert.equal(filters.status, 200); assert(!('secret' in filters.body.data)); assert.equal(filters.body.data.generation, 1); assert.equal(filters.body.data.revision, 2);
    assert.equal((await f.send(f.path + '/webhook', f.create)).body.data.secret, secret);
    const rotation = { requestId: randomUUID(), revision: 2, rotate: true }, rotated = await f.send(f.path + '/webhook', rotation);
    assert.equal(rotated.status, 201); assert.notEqual(rotated.body.data.secret, secret); assert.equal(rotated.body.data.generation, 2);
    assert.equal((await f.event(secret, 'old-secret')).status, 401); assert.equal((await f.event(secret)).status, 401);
    const oldRetry = await f.send(f.path + '/webhook', f.create);
    assert.equal(oldRetry.status, 200); assert.equal(oldRetry.body.data.rotated, true); assert.match(oldRetry.body.data.message, /rotated/); assert(!('secret' in oldRetry.body.data));
    await f.restart(); assert.deepEqual((await f.send(f.path + '/webhook', rotation)).body, rotated.body);
    assert.deepEqual((await f.event(rotated.body.data.secret)).body, accepted);
    const member = (await f.send('/api/session', undefined, 'user_collaborator')).body.data.accountId;
    db.prepare("UPDATE record_scopes SET account=?,visibility='owner' WHERE table_name='routine_occurrences'").run(member);
    assert.equal((await f.send(f.path + '/webhook')).body.data.receipts.length, 0); assert.equal((await f.event(rotated.body.data.secret)).status, 403);
    db.prepare("UPDATE record_scopes SET visibility='project' WHERE table_name='routine_occurrences'").run();
    assert.equal(db.prepare('SELECT count(*) n FROM routine_occurrences').get()!.n, 1); assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(), []);
  } finally { globalThis.fetch = originalFetch; db.close(); await other.close(); await f.close(); }
});

test('canonical routine webhook conceals missing and inactive targets until signature verification', async () => {
  const f = await routineWebhookFixture();
  try {
    const denied = { status: 401, body: { ok: false, error: { code: 'invalid_webhook', message: 'Webhook signature does not match.', retryable: false } } };
    const expectDenied = (response: { status: number; body: any }) => {
      const { requestId, ...error } = response.body.error;
      assert.match(requestId, /^[a-f0-9-]{36}$/);
      assert.deepEqual({ ...response, body: { ...response.body, error } }, denied);
    };
    const badSignature = () => f.event('wrong-secret');
    expectDenied(await badSignature());
    for (const header of ['X-Minarbot-Time', 'X-Minarbot-Id', 'X-Minarbot-Signature']) {
      for (const value of ['', 'invalid!', undefined]) expectDenied(await f.event(undefined, undefined, undefined, undefined, { [header]: value }));
    }
    for (const offset of [-301, 301]) expectDenied(await f.event(undefined, undefined, undefined, String(Math.floor(Date.now() / 1000) + offset)));
    for (const route of [f.path.replace(f.routine, randomUUID()), f.path.replace(f.project, randomUUID()), f.path.replace(f.project, f.other)]) {
      expectDenied(await f.event(undefined, undefined, undefined, undefined, {}, route + '/webhook-events'));
    }
    const routine = randomUUID(), noHook = `/api/projects/${f.project}/routines/${routine}/webhook-events`;
    assert.equal((await f.send(`/api/projects/${f.project}/agents/${f.agent}/routines`, { ...f.configuration, requestId: routine, id: routine })).status, 201);
    expectDenied(await f.event(undefined, undefined, undefined, undefined, {}, noHook));
    expectDenied(await f.event('minarbot-routine-webhook-dummy-secret-v1', undefined, '{', undefined, {}, noHook));
    f.webhookFault.read = true;
    expectDenied(await f.event(undefined, undefined, undefined, undefined, {}, noHook));
    for (const extra of [{}, { 'X-Minarbot-Signature': '' }, { 'X-Minarbot-Signature': '0'.repeat(64) }]) {
      const before = f.stderr().length;
      expectDenied(await f.event(undefined, undefined, undefined, undefined, extra));
      await until(() => f.stderr().length > before);
      assert.equal(f.stderr().slice(before).trim(), `Webhook keystore is unavailable for routine ${f.routine}.`);
    }
    assert.equal((await f.send(f.path + '/webhook')).body.data.keystore, 'unavailable');
    assert.equal((await f.send(f.path + '/webhook', f.create)).status, 503);
    assert(!f.stderr().includes(f.hook.data.secret));
    assert(!f.stderr().includes('PRIVATE_'));
    f.webhookFault.read = false;
    assert.equal((await f.send(f.path + '/webhook')).body.data.keystore, 'ok');
    for (const [revision, enabled, trigger] of [[1, false, 'webhook'], [2, true, 'schedule']] as const) {
      const requestId = randomUUID();
      assert.equal((await f.send(f.path + '/edit', { ...f.configuration, requestId, id: requestId, revision, enabled, trigger })).status, 200);
      expectDenied(await badSignature());
      const signed = await f.event();
      assert.equal(signed.status, 409); assert.equal(signed.body.error.code, 'webhook_inactive');
      assert.equal(signed.body.error.message, 'Webhook routine is inactive.');
    }
  } finally { await f.close(); }
});

test('canonical routine webhook performs one keyring read for configured and missing targets', async () => {
  const f = await routineWebhookFixture();
  try {
    const read = async (route: string, extra: Record<string, string | undefined> = {}) => {
      f.webhookFault.reads.length = 0;
      assert.equal((await f.event('wrong-secret', undefined, undefined, undefined, extra, route)).status, 401);
      assert.equal(f.webhookFault.reads.length, 1);
      return f.webhookFault.reads[0];
    };
    const configured = await read(f.path + '/webhook-events');
    const missing = await read(f.path.replace(f.routine, randomUUID()) + '/webhook-events');
    const otherMissing = await read(f.path.replace(f.project, randomUUID()) + '/webhook-events');
    assert.equal(missing.service, configured.service);
    assert.notEqual(missing.name, configured.name);
    assert.deepEqual(otherMissing, missing);
    assert(!f.webhookKeys.values.has(`${missing.service}/${missing.name}`));
    for (const extra of [{ 'X-Minarbot-Signature': '' }, { 'X-Minarbot-Id': 'invalid!' }, { 'X-Minarbot-Time': undefined }]) {
      assert.deepEqual(await read(f.path + '/webhook-events', extra), configured);
      assert.deepEqual(await read(f.path.replace(f.routine, randomUUID()) + '/webhook-events', extra), missing);
    }
  } finally { await f.close(); }
});

test('canonical routine webhook rejects invalid deliveries and records ignored receipts across filter changes', async () => {
  const f = await routineWebhookFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
  try {
    const secret = f.hook.data.secret, body = '{"event":"ready","payload":{"count":4}}', time = String(Math.floor(Date.now() / 1000));
    for (const header of ['X-Minarbot-Time','X-Minarbot-Id','X-Minarbot-Signature']) assert.equal((await f.event(secret, 'bad', body, time, { [header]: '' })).status, 401);
    assert.equal((await f.event(secret, 'bad', body, time, { 'X-Minarbot-Signature': '0'.repeat(64) })).status, 401);
    // Fresh whole seconds per request; ±302 keeps a margin over the server's fractional 300 s window.
    for (const offset of [-302,302]) assert.equal((await f.event(secret, 'expired', body, String(Math.floor(Date.now() / 1000) + offset))).status, 401);
    assert.equal((await f.event(secret, 'large', ' '.repeat(65537))).status, 413);
    assert.equal((await f.event(secret, 'json', '{')).status, 400); assert.equal((await f.event(secret, 'utf8', Buffer.from([0xff]))).status, 400);
    assert.equal((await f.event(secret, 'content', body, time, { 'Content-Type': 'text/plain' })).status, 415);
    assert.equal((await f.event(secret, 'origin', body, time, { Origin: f.origin() })).status, 403);
    assert.equal((await f.event(secret, 'selector', body, time, { 'X-Minarbot-Project': f.project })).status, 404);
    const changed = '{"event":"ready","payload":{"count":5}}', ignored = await f.event(secret, 'ignored', changed);
    assert.equal(ignored.status, 202); assert.equal(ignored.body.data.outcome, 'ignored'); assert.equal(db.prepare('SELECT count(*) n FROM routine_occurrences').get()!.n, 0);
    assert.equal((await f.send(f.path + '/webhook')).body.data.receipts[0].outcome, 'ignored');
    assert.equal((await f.send(f.path + '/webhook', { requestId: randomUUID(), revision: 1, rotate: false, filters: [] })).status, 200);
    assert.deepEqual((await f.event(secret, 'ignored', changed)).body, ignored.body); assert.equal((await f.event(secret, 'ignored', body)).status, 409);
    for (const [revision, enabled, trigger] of [[1, false, 'webhook'], [2, true, 'schedule']] as const) {
      const requestId = randomUUID(); assert.equal((await f.send(f.path + '/edit', { ...f.configuration, requestId, id: requestId, revision, enabled, trigger })).status, 200);
      assert.equal((await f.event(secret, 'inactive-' + revision)).status, 409); assert.equal((await f.send(f.path + '/webhook')).body.data.enabled, false);
    }
    assert.equal((await f.send(f.path + '/webhook', f.create)).body.data.secret, secret, 'Current-generation exact retry survives trigger edits');
    assert.equal(db.prepare('SELECT count(*) n FROM routine_occurrences').get()!.n, 0); assert.equal(db.prepare('SELECT count(*) n FROM routine_webhook_receipts').get()!.n, 1);
  } finally { db.close(); await f.close(); }
});

test('canonical routine webhook settings require current owner while assigned collaborator can edit', async () => {
  const f = await routineWebhookFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
  try {
    const member = (await f.send('/api/session', undefined, 'user_collaborator')).body.data.accountId, owner = (await f.send('/api/session')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(f.project, member); db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(f.project, member, f.agent);
    const editId = randomUUID(); assert.equal((await f.send(f.path + '/edit', { ...f.configuration, requestId: editId, id: editId, revision: 1 }, 'user_collaborator')).status, 200);
    assert.equal((await f.send(f.path + '/webhook', undefined, 'user_collaborator')).status, 403);
    for (const command of [f.create, { requestId: randomUUID(), revision: 1, rotate: true }]) assert.equal((await f.send(f.path + '/webhook', command, 'user_collaborator')).status, 403);
    const foreign = f.path.replace(f.project, f.other);
    assert.equal((await f.send(foreign + '/webhook')).status, 404); assert.equal((await f.send(foreign + '/webhook', { requestId: randomUUID(), revision: 1, rotate: true })).status, 404);
    assert.equal((await f.event(f.hook.data.secret, 'foreign', undefined, undefined, {}, foreign + '/webhook-events')).status, 401);
    for (const extra of [{ unexpected: true }, { rotate: 'yes' }, { revision: -1 }, { filters: [{}] }]) assert.equal((await f.send(f.path + '/webhook', { ...f.create, requestId: randomUUID(), ...extra })).status, 400);
    const conflict = await f.send(f.path + '/webhook', { ...f.create, rotate: false }); assert.equal(conflict.status, 409); assert.equal(conflict.body.error.code, 'payload_conflict');
    db.prepare('UPDATE project_members SET deleted=1,grant_revision=grant_revision+1 WHERE project=? AND account=?').run(f.project, owner);
    assert.equal((await f.send(f.path + '/webhook')).status, 404); assert.equal((await f.send(f.path + '/webhook', f.create)).status, 404); assert.equal((await f.event()).status, 403);
    db.prepare('UPDATE project_members SET deleted=0 WHERE project=? AND account=?').run(f.project, owner); db.prepare('UPDATE accounts SET blocked=1 WHERE id=?').run(owner);
    assert.equal((await f.send(f.path + '/webhook')).status, 401); assert.equal((await f.event()).status, 403);
    assert.equal(db.prepare('SELECT count(*) n FROM routine_occurrences').get()!.n, 0);
  } finally { db.close(); await f.close(); }
});

test('canonical routine webhook keyring and DB failures leave no active reference or partial admission and exclude secrets', async () => {
  const f = await routineWebhookFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
  try {
    const row = db.prepare('SELECT * FROM routine_webhooks').get()!, rotate = { requestId: randomUUID(), revision: 1, rotate: true };
    f.webhookFault.write = true; assert.equal((await f.send(f.path + '/webhook', rotate)).status, 503); f.webhookFault.write = false;
    f.webhookFault.read = true; assert.equal((await f.send(f.path + '/webhook', rotate)).status, 503); f.webhookFault.read = false;
    assert.deepEqual(db.prepare('SELECT * FROM routine_webhooks').get(), row); assert.equal(db.prepare('SELECT 1 FROM receipts WHERE request_id=?').get(rotate.requestId), undefined);
    const newRoutine = randomUUID(), detail = `/api/projects/${f.project}/routines/${newRoutine}`;
    assert.equal((await f.send(`/api/projects/${f.project}/agents/${f.agent}/routines`, { ...f.configuration, requestId: newRoutine, id: newRoutine })).status, 201);
    const create = { requestId: randomUUID(), revision: 0, rotate: true };
    f.webhookFault.read = true; assert.equal((await f.send(detail + '/webhook', create)).status, 503); f.webhookFault.read = false;
    assert.equal(db.prepare('SELECT 1 FROM routine_webhooks WHERE routine=?').get(newRoutine), undefined); assert.equal((await f.send(detail + '/webhook')).body.data.generation, 0);
    db.exec("CREATE TRIGGER fail_webhook_command BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'retained:%/webhook' BEGIN SELECT RAISE(ABORT,'webhook DB failure'); END");
    assert.equal((await f.send(detail + '/webhook', create)).status, 500); assert.equal(db.prepare('SELECT 1 FROM routine_webhooks WHERE routine=?').get(newRoutine), undefined);
    assert.equal((await f.send(f.path + '/webhook', rotate)).status, 500); assert.deepEqual(db.prepare('SELECT * FROM routine_webhooks WHERE routine=?').get(f.routine), row);
    db.exec('DROP TRIGGER fail_webhook_command');
    for (const failure of ["CREATE TRIGGER fail_webhook_event BEFORE INSERT ON routine_webhook_receipts BEGIN SELECT RAISE(ABORT,'event failure'); END", "CREATE TRIGGER fail_webhook_event BEFORE INSERT ON sync_events WHEN NEW.record_kind='routine_occurrence' BEGIN SELECT RAISE(ABORT,'event failure'); END"]) {
      const tables = ['receipts','tasks','turns','threads','record_scopes','sync_events','routine_occurrences','routine_steps'], counts = tables.map(table => db.prepare(`SELECT count(*) n FROM ${table}`).get()!.n);
      db.exec(failure); assert.equal((await f.event()).status, 500); db.exec('DROP TRIGGER fail_webhook_event');
      assert.deepEqual(tables.map(table => db.prepare(`SELECT count(*) n FROM ${table}`).get()!.n), counts); assert.equal(db.prepare('SELECT count(*) n FROM routine_webhook_receipts').get()!.n, 0);
    }
    assert.equal((await f.event()).status, 202);
    const reads = [(await f.send(f.path)).body, (await f.send(f.path + '/webhook')).body, (await f.send(f.path + '/runs')).body, (await f.send('/api/sync?projectId=' + f.project)).body], snapshot = await workspaceSnapshot(db);
    try { for (const value of f.webhookKeys.values.values()) assert(!readFileSync(snapshot.file).includes(Buffer.from(value))); } finally { snapshot.cleanup(); }
    for (const value of f.webhookKeys.values.values()) {
      assert(!JSON.stringify([reads, f.stdout(), f.stderr()]).includes(value)); assert(!readFileSync(join(f.dir, 'canonical.sqlite')).includes(Buffer.from(value)));
      for (const table of db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'").all()) assert(!JSON.stringify(db.prepare(`SELECT * FROM "${table.name}"`).all()).includes(value), String(table.name));
    }
    assert.equal((await f.send(f.path + '/webhook', f.create)).body.data.secret, f.hook.data.secret); assert.equal((await f.send(detail + '/webhook', create)).status, 201);
    assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(), []);
  } finally { db.close(); await f.close(); }
});
