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

for(const name of ['no_update','report_failure'] as const) test(`canonical routine ${name} recovers terminal result after home SIGKILL and server restart without fan-out`,async()=>{
  const f=await rustWorkflowConfigurationFixture(),routine=randomUUID(),detail=`/api/projects/${f.project}/routines/${routine}`,db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
  const reason=name==='no_update'?'Checked all sources; nothing changed.':'Required service access is missing.';
  let calls=0;
  const provider=createServer(async(req,res)=>{
    let body='';for await(const chunk of req)body+=chunk;
    const {snapshot}=JSON.parse(body);assert(snapshot.tools.some((tool:any)=>tool.function.name===name),'Actual routine step offers outcome tools');calls++;
    res.end(JSON.stringify({toolCall:{id:'outcome',function:{name,arguments:JSON.stringify({reason:`  ${reason}  `})}}}));
  });
  await new Promise<void>(resolve=>provider.listen(0,'127.0.0.1',resolve));
  const address=provider.address();if(!address||typeof address==='string')throw new Error('Missing provider fixture address');const providerOrigin=`http://127.0.0.1:${address.port}`;
  try{
    const connection=f.runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data.find((row:any)=>row.id===f.home.id),peer=randomUUID(),binding=randomUUID(),owner=(await f.send('/api/session')).body.data.accountId;
    db.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','routine-outcome','Fixture','ready',1)").run(binding,home.id,owner);
    db.prepare('UPDATE agent_authority SET provider_binding=?,revision=revision+1 WHERE agent=?').run(binding,f.agent);
    assert.equal((await f.send('/api/agents',{requestId:peer,id:peer,name:'Downstream',model:'fixture',backupModel:'',instructions:'Help',threadLimit:2,projectId:f.project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:{bindingId:binding,expectedRevision:1}})).status,201);
    const create={requestId:routine,id:routine,revision:0,name:'Outcome flow',prompt:'Check for changes',nextAt:Date.now()+600000,intervalSeconds:null,enabled:true};
    assert.equal((await f.send(`/api/projects/${f.project}/agents/${f.agent}/routines`,create)).status,201);
    const flow=randomUUID();assert.equal((await f.send(detail+'/flow',{requestId:flow,id:flow,revision:0,links:[{from:f.agent,to:peer}]})).status,200);
    const edit=randomUUID();assert.equal((await f.send(detail+'/edit',{...create,requestId:edit,id:edit,revision:1,nextAt:Date.now()-1000})).status,200);
    let run:any;await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run?.state==='admitted';});
    await connection.stop();
    await crashRustDeviceTask(f.hostOptions, run.taskId, providerOrigin, 'tool-reply');
    assert.equal(calls,1);
    await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run.state===(name==='no_update'?'done':'failed');});
    assert.deepEqual(run.steps.map((step:any)=>[step.agentId,step.state,step.taskId]).sort(),[[f.agent,name==='no_update'?'skipped':'failed',run.taskId],[peer,'skipped',null]].sort());
    assert.equal(run.steps.find((step:any)=>step.agentId===f.agent).preview,reason);
    assert.deepEqual({...db.prepare('SELECT status,no_update,reply,error FROM turns WHERE id=?').get(run.taskId)!},name==='no_update'?{status:'done',no_update:1,reply:reason,error:null}:{status:'failed',no_update:0,reply:null,error:reason});
    assert.equal(db.prepare('SELECT stopped FROM execution_owners WHERE task=?').get(run.taskId)!.stopped,1);
    const saved=db.prepare("SELECT r.action,r.content_json FROM task_results r JOIN task_actions a ON a.id=r.action WHERE r.task=? AND a.action_kind='canonical'").get(run.taskId)!;
    await f.restart();
    for(let restart=0;restart<2;restart++){
      const resumed=await runRustDeviceTask(f.hostOptions, run.taskId, providerOrigin);
      assert.equal(resumed.state,name==='no_update'?'completed':'failed');assert.equal(calls,1);
    }
    assert.equal(db.prepare('SELECT content_json FROM task_results WHERE action=?').get(saved.action)!.content_json,saved.content_json);
    assert.equal(db.prepare('SELECT count(*) n FROM task_actions WHERE task=?').get(run.taskId)!.n,2);
    assert.equal(db.prepare('SELECT count(*) n FROM task_results WHERE task=?').get(run.taskId)!.n,2);
    assert.equal(db.prepare('SELECT count(*) n FROM tasks WHERE project=?').get(f.project)!.n,1);
    assert.deepEqual((await f.send(detail+'/runs')).body.data.items[0],run);
  }finally{
    provider.closeAllConnections();await new Promise<void>(resolve=>provider.close(()=>resolve()));db.close();await f.close();
  }
});

test('canonical routine mixed fan-in waits for success and keeps quiet and failed outcomes distinct',async()=>{
  const f=await workflowConfigurationFixture(),routine=randomUUID(),detail=`/api/projects/${f.project}/routines/${routine}`,db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
  try{
    const connection=f.runtime.connect();await until(()=>connection.state().phase==='connected');
    const home=(await f.send('/api/devices')).body.data.find((row:any)=>row.id===f.home.id),binding=randomUUID(),owner=(await f.send('/api/session')).body.data.accountId;
    db.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','routine-fan-in','Fixture','ready',1)").run(binding,home.id,owner);
    db.prepare('UPDATE agent_authority SET provider_binding=?,revision=revision+1 WHERE agent=?').run(binding,f.agent);
    const [quiet,failed,good,joined]=Array.from({length:4},()=>randomUUID());
    for(const agent of [quiet,failed,good,joined])assert.equal((await f.send('/api/agents',{requestId:agent,id:agent,name:'Flow member',model:'fixture',backupModel:'',instructions:'Help',threadLimit:2,projectId:f.project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:{bindingId:binding,expectedRevision:1}})).status,201);
    const create={requestId:routine,id:routine,revision:0,name:'Mixed flow',prompt:'Root work',nextAt:Date.now()+600000,intervalSeconds:null,enabled:true};
    assert.equal((await f.send(`/api/projects/${f.project}/agents/${f.agent}/routines`,create)).status,201);
    const flow=randomUUID(),links=[quiet,failed,good].flatMap(agent=>[{from:f.agent,to:agent},{from:agent,to:joined}]);
    assert.equal((await f.send(detail+'/flow',{requestId:flow,id:flow,revision:0,links})).status,200);
    const edit=randomUUID();assert.equal((await f.send(detail+'/edit',{...create,requestId:edit,id:edit,revision:1,nextAt:Date.now()-1000})).status,200);
    let run:any;await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run?.state==='admitted';});
    assert.equal((await f.runtime.runTask(run.taskId,()=>({text:'Root evidence'}))).state,'completed');
    await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return [quiet,failed,good].every(agent=>run.steps.find((step:any)=>step.agentId===agent)?.taskId);});
    for(const [agent,name,reason] of [[quiet,'no_update','Quiet check found no change'],[failed,'report_failure','One source is unavailable']]){
      const taskId=run.steps.find((step:any)=>step.agentId===agent).taskId;
      assert.equal((await f.runtime.runTask(taskId,context=>{
        assert((context.snapshot.tools as any[]).some(tool=>tool.function.name===name),'Downstream step offers outcome tools');
        return {toolCall:{id:name,function:{name,arguments:JSON.stringify({reason})}}};
      })).state,name==='no_update'?'completed':'failed');
    }
    await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run.steps.find((step:any)=>step.agentId===failed)?.state==='failed';});
    assert.equal(run.steps.find((step:any)=>step.agentId===quiet).state,'skipped');
    assert.equal(run.steps.find((step:any)=>step.agentId===joined).taskId,null,'Fan-in waits for every upstream result');
    const goodTask=run.steps.find((step:any)=>step.agentId===good).taskId;
    assert.equal((await f.runtime.runTask(goodTask,()=>({text:'Actionable result'}))).state,'completed');
    await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run.steps.find((step:any)=>step.agentId===joined)?.taskId;});
    const joinedTask=run.steps.find((step:any)=>step.agentId===joined).taskId;
    assert.equal((await f.runtime.runTask(joinedTask,context=>{
      const prompt=(context.snapshot.conversation as {message:{text:string}}).message.text;
      assert.match(prompt,/Actionable result/);assert.match(prompt,/"status":"skipped"/);assert.match(prompt,/One source is unavailable/);
      return {text:'Merged actionable result'};
    })).state,'completed');
    await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run.state==='failed';});
    assert.equal(run.steps.find((step:any)=>step.agentId===joined).state,'done');
    assert.equal(db.prepare('SELECT count(*) n FROM tasks WHERE project=? AND agent=?').get(f.project,joined)!.n,1);
  }finally{db.close();await f.close();}
});

test('canonical routine keeps no-reply descendants executable after completion and fences revoked root authority',async()=>{
  const f=await workflowConfigurationFixture(),routine=randomUUID(),detail=`/api/projects/${f.project}/routines/${routine}`,db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
  try{
    const connection=f.runtime.connect();await until(()=>connection.state().phase==='connected');
    const binding=randomUUID(),peer=randomUUID(),owner=(await f.send('/api/session')).body.data.accountId,home=(await f.send('/api/devices')).body.data[0];
    db.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','routine-fixture','Fixture','ready',1)").run(binding,f.home.id,owner);
    db.prepare('UPDATE agent_authority SET provider_binding=?,revision=revision+1 WHERE agent=?').run(binding,f.agent);
    assert.equal((await f.send('/api/agents',{requestId:peer,id:peer,name:'Teammate',model:'fixture',backupModel:'',instructions:'Help',threadLimit:2,projectId:f.project,homeDeviceId:home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:home.revision,providerBinding:null})).status,201);
    assert.equal((await f.send(`/api/projects/${f.project}/agents/${f.agent}/routines`,{requestId:routine,id:routine,revision:0,name:'Background work',prompt:'Delegate without waiting',nextAt:Date.now()-1000,intervalSeconds:null,enabled:true})).status,201);
    let run:any,calls=0;await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run?.state==='admitted';});
    assert.equal((await f.runtime.runTask(run.taskId,()=>++calls===1?{toolCall:{id:'delegate',function:{name:'send_message',arguments:JSON.stringify({recipients:[peer],text:'Background work',expects_reply:false})}}}:{text:'Parent done'})).state,'completed');
    await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run.state==='done';});
    const child=String(db.prepare('SELECT id FROM turns WHERE parent=?').get(run.taskId)!.id),owned=await f.runtime.startTask(child);
    await f.runtime.taskContext(child,owned.ownership,owned.revision);
    db.prepare('UPDATE agent_projects SET deleted=1 WHERE project=? AND agent=?').run(f.project,f.agent);
    await assert.rejects(f.runtime.taskContext(child,owned.ownership,owned.revision),{code:'execution_denied'});
    assert.equal(db.prepare('SELECT reply FROM turns WHERE id=?').get(run.taskId)!.reply,'Parent done');
  }finally{db.close();await f.close();}
});

test('canonical routine runtime rechecks grants and rolls back failed task admissions without consuming occurrence',async()=>{
  const f=await workflowConfigurationFixture(),routine=randomUUID(),detail=`/api/projects/${f.project}/routines/${routine}`,db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
  try{
    const collaborator=(await f.send('/api/session',undefined,'user_collaborator')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(f.project,collaborator);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(f.project,collaborator,f.agent);
    const command={requestId:routine,id:routine,revision:0,name:'Revocable',prompt:'Check',nextAt:Date.now()-1000,intervalSeconds:null,enabled:true};
    assert.equal((await f.send(`/api/projects/${f.project}/agents/${f.agent}/routines`,command,'user_collaborator')).status,201);
    let run:any;await until(async()=>{run=(await f.send(detail+'/runs',undefined,'user_collaborator')).body.data.items[0];return run?.state==='pending';});
    const occurrenceId=run.id;
    db.prepare('UPDATE collaborator_agents SET enabled=0,revision=2 WHERE project=? AND account=?').run(f.project,collaborator);
    const connection=f.runtime.connect();await until(()=>connection.state().phase==='connected');
    assert.equal(db.prepare('SELECT task FROM routine_occurrences WHERE id=?').get(occurrenceId)!.task,null);
    assert.equal((await f.send(detail+'/runs',undefined,'user_collaborator')).status,404);
    db.prepare('UPDATE collaborator_agents SET enabled=1,revision=3 WHERE project=? AND account=?').run(f.project,collaborator);
    db.exec("CREATE TRIGGER fail_routine_admission BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'tasks:routine-step:%' BEGIN SELECT RAISE(ABORT,'synthetic routine receipt failure'); END");
    const rebind=randomUUID();assert.equal((await f.send(detail+'/edit',{...command,requestId:rebind,id:rebind,revision:1},'user_collaborator')).status,200);
    await until(() => f.stderr().includes('Project automation failed:'));
    assert.equal(db.prepare('SELECT task FROM routine_occurrences WHERE id=?').get(occurrenceId)!.task,null,'Regrant never revives old schedule authority');
    assert.equal(db.prepare('SELECT count(*) n FROM routine_steps').get()!.n,0);assert.equal(db.prepare('SELECT count(*) n FROM tasks').get()!.n,0);assert.equal((await f.send(detail)).body.data.enabled,1);
    db.exec('DROP TRIGGER fail_routine_admission');
    let accepted:any; await until(async()=>{accepted=(await f.send(detail+'/runs',undefined,'user_collaborator')).body.data.items.find((row:any)=>row.state==='admitted');return !!accepted;});
    assert.equal((await f.send(`/api/tasks/${accepted.taskId}`,undefined,'user_collaborator')).body.receipt.actorId,collaborator);
    db.prepare('UPDATE collaborator_agents SET enabled=0,revision=4 WHERE project=? AND account=?').run(f.project,collaborator);
    await assert.rejects(f.runtime.startTask(accepted.taskId),error=>error instanceof ProtocolError&&error.code==='execution_denied');
    assert.equal(db.prepare('SELECT count(*) n FROM task_actions').get()!.n,0);assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(),[]);
  }finally{db.close();await f.close();}
});

test('canonical routine edit cancels unstarted scheduled work and retains accepted evidence',async()=>{
  const f=await rustWorkflowConfigurationFixture(),routine=randomUUID(),list=`/api/projects/${f.project}/agents/${f.agent}/routines`,detail=`/api/projects/${f.project}/routines/${routine}`;
  try{
    const connection=f.runtime.connect();await until(()=>connection.state().phase==='connected');
    const command={requestId:routine,id:routine,revision:0,name:'Disable queued',prompt:'Never start after disable',nextAt:Date.now()-1000,intervalSeconds:300,enabled:true};
    assert.equal((await f.send(list,command)).status,201);
    let run:any;await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run?.state==='admitted';});
    const task=await f.send(`/api/tasks/${run.taskId}`);assert.equal(task.body.data.state,'queued');
    const current=(await f.send(detail)).body.data,edit=randomUUID(),saved=await f.send(detail+'/edit',{...command,requestId:edit,id:edit,revision:current.revision,nextAt:current.next_at,enabled:false});
    assert.equal(saved.status,200,JSON.stringify(saved.body));
    assert.equal((await f.send(`/api/tasks/${run.taskId}`)).body.data.state,'cancelled');
    assert.deepEqual((await f.send(`/api/tasks/${run.taskId}`)).body.receipt,task.body.receipt);
    await f.restart();assert.equal((await f.send(`/api/tasks/${run.taskId}`)).body.data.state,'cancelled');
  }finally{await f.close();}
});

test('canonical routine runtime preserves started snapshots through edits and restore holds unfinished occurrences',async()=>{
  const f=await workflowConfigurationFixture(),routine=randomUUID(),list=`/api/projects/${f.project}/agents/${f.agent}/routines`,detail=`/api/projects/${f.project}/routines/${routine}`;
  try{
    const connection=f.runtime.connect();await until(()=>connection.state().phase==='connected');
    const command={requestId:routine,id:routine,revision:0,name:'Started',prompt:'Retain started work',nextAt:Date.now()-1000,intervalSeconds:300,enabled:true};
    assert.equal((await f.send(list,command)).status,201);
    let run:any;await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run?.state==='admitted';});
    const owned=await f.runtime.startTask(run.taskId),current=(await f.send(detail)).body.data,edit=randomUUID();
    assert.equal((await f.send(detail+'/edit',{...command,requestId:edit,id:edit,revision:current.revision,nextAt:current.next_at,enabled:false})).status,200);
    assert.equal((await f.send(`/api/tasks/${run.taskId}`)).body.data.state,'running','Configuration edits keep started snapshot under task controls');
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite')),backup=await workspaceSnapshot(db);
    db.close();await connection.stop();
    try{await f.restoreFrom(backup.file);}finally{backup.cleanup();}
    const restored=fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try{
      const row=restored.prepare('SELECT * FROM routine_occurrences WHERE id=?').get(run.id)!;
      assert.equal(row.state,'blocked');assert.equal(row.task,run.taskId);assert.equal(row.due_at,run.dueAt);assert.equal(row.schedule_revision,run.scheduleRevision);
      assert.equal(restored.prepare('SELECT enabled FROM routines WHERE id=?').get(routine)!.enabled,0);
      assert.equal(restored.prepare('SELECT state FROM tasks WHERE id=?').get(run.taskId)!.state,'uncertain');
      await f.restart();assert.equal(restored.prepare('SELECT count(*) n FROM routine_occurrences').get()!.n,1);
      assert.equal(restored.prepare('SELECT state FROM routine_occurrences WHERE id=?').get(run.id)!.state,'blocked');assert.deepEqual(restored.prepare('PRAGMA foreign_key_check').all(),[]);
    }finally{restored.close();}
    await assert.rejects(f.runtime.taskContext(run.taskId,owned.ownership,owned.revision));
  }finally{await f.close();}
});

test('canonical routine cancellation settles terminal occurrence after task cancel',async()=>{
  const f=await rustWorkflowConfigurationFixture(),routine=randomUUID(),list=`/api/projects/${f.project}/agents/${f.agent}/routines`,detail=`/api/projects/${f.project}/routines/${routine}`;
  try{
    const connection=f.runtime.connect();await until(()=>connection.state().phase==='connected');
    const due=Date.now()-1000,command={requestId:routine,id:routine,revision:0,name:'Cancel',prompt:'Cancel me',nextAt:due,intervalSeconds:300,enabled:true};
    assert.equal((await f.send(list,command)).status,201);let run:any;await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run?.state==='admitted';});
    const task=(await f.send(`/api/tasks/${run.taskId}`)).body.data,control=randomUUID();
    assert.equal((await f.send(`/api/tasks/${run.taskId}/control`,{requestId:control,expectedRevision:task.revision,operation:'cancel'})).status,200);
    await until(async()=>{run=(await f.send(detail+'/runs')).body.data.items[0];return run?.state==='failed';});
    assert.equal((await f.send(`/api/tasks/${run.taskId}`)).body.data.state,'cancelled');
    const db=fixtureDatabase(join(f.dir,'canonical.sqlite'));try{assert.equal(db.prepare('SELECT state FROM routine_steps WHERE occurrence=?').get(run.id)!.state,'failed');}finally{db.close();}
    await f.restart();assert.equal((await f.send(detail+'/runs')).body.data.items[0].state,'failed');
  }finally{await f.close();}
});

test('canonical routines recheck assignments private mappings flow agents and receipt rollback',async()=>{
  const f=await rustWorkflowConfigurationFixture(),list=`/api/projects/${f.project}/agents/${f.agent}/routines`,routine=randomUUID(),detail=`/api/projects/${f.project}/routines/${routine}`,db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
  const command={requestId:routine,id:routine,revision:0,name:'Routine',prompt:'Check',nextAt:Date.now()+600000,intervalSeconds:null,enabled:false};
  try{
    for(const fields of [{trigger:['webhook']},{trigger:{}},{nextAt:-1},{intervalSeconds:1.5},{prompt:''}]){
      const invalid=randomUUID();assert.equal((await f.send(list,{...command,...fields,requestId:invalid,id:invalid})).status,400);
    }
    assert.deepEqual((await f.send(list)).body.data.items,[],'Invalid bodies write no configuration');
    assert.equal((await f.send(list,command)).status,201);
    const collaborator=(await f.send('/api/session',undefined,'user_collaborator')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(f.project,collaborator);
    assert.equal((await f.send(detail,undefined,'user_collaborator')).status,404);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(f.project,collaborator,f.agent);
    db.prepare('INSERT INTO response_policies VALUES (?,?,?,0,1)').run(f.project,collaborator,f.agent);
    assert.equal((await f.send(detail,undefined,'user_collaborator')).status,200,'Response OFF retains assignment');
    const editId=randomUUID(),edit={...command,requestId:editId,id:editId,revision:1,name:'Collaborator'};
    const saved=await f.send(detail+'/edit',edit,'user_collaborator');assert.equal(saved.status,200);
    db.prepare('UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?').run(f.project,collaborator);
    assert.equal((await f.send(detail+'/edit',edit,'user_collaborator')).status,404,'Revoked grants cannot read old receipts');
    db.prepare('UPDATE collaborator_agents SET enabled=1,revision=revision+1 WHERE project=? AND account=?').run(f.project,collaborator);
    const owner=(await f.send('/api/session')).body.data.accountId;
    db.prepare("UPDATE record_scopes SET account=?,visibility='owner' WHERE table_name='routines' AND key_json=?").run(collaborator,JSON.stringify([routine]));
    assert.equal((await f.send(detail)).status,404,'Owner cannot read another human private root');
    assert.equal((await f.send(detail,undefined,'user_collaborator')).status,200);
    db.prepare("UPDATE record_scopes SET account=?,visibility='project' WHERE table_name='routines' AND key_json=?").run(owner,JSON.stringify([routine]));
    const teammate=randomUUID();
    assert.equal((await f.send('/api/agents',{requestId:teammate,id:teammate,name:'Teammate',model:'fixture',backupModel:'',instructions:'Help',threadLimit:2,projectId:f.project,homeDeviceId:f.home.id,expectedProjectRevision:1,expectedHomeDeviceRevision:f.home.revision,providerBinding:null})).status,201);
    const flowId=randomUUID(),flow={requestId:flowId,id:flowId,revision:0,links:[{from:f.agent,to:teammate}]};
    assert.equal((await f.send(detail+'/flow',flow,'user_collaborator')).status,404,'Every flow agent needs assignment');
    const bad=randomUUID();assert.equal((await f.send(detail+'/flow',{...flow,links:[{from:f.agent,to:bad}]})).status,400);
    assert.equal((await f.send(detail+'/flow',flow)).status,200);
    db.prepare("UPDATE record_scopes SET project=? WHERE table_name='routine_flows' AND key_json=?").run(f.other,JSON.stringify([routine]));
    assert.equal((await f.send(detail+'/flow',flow)).status,404,'Receipt cannot expose a foreign flow');
    db.prepare("UPDATE record_scopes SET project=? WHERE table_name='routine_flows' AND key_json=?").run(f.project,JSON.stringify([routine]));
    db.exec("CREATE TRIGGER fail_routine_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE '%/edit' BEGIN SELECT RAISE(ABORT,'synthetic routine receipt failure'); END");
    const change=randomUUID(),edit2={...edit,requestId:change,id:change,revision:2,name:'Atomic'};
    assert.equal((await f.send(detail+'/edit',edit2)).status,500);
    assert.equal((await f.send(detail)).body.data.name,'Collaborator');
    db.exec('DROP TRIGGER fail_routine_receipt');assert.equal((await f.send(detail+'/edit',edit2)).status,200);
    const short=randomUUID();assert.equal((await f.send(detail+'/edit',{...edit2,requestId:short,id:short,revision:3,intervalSeconds:60})).status,400);
  }finally{db.close();await f.close();}
});

test('canonical routine wall-clock HTTP keeps exact retries and pending identities across restart and catch-up admission', async () => {
  const f = await rustWorkflowConfigurationFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite')), originalFetch = globalThis.fetch;
  const list = `/api/projects/${f.project}/agents/${f.agent}/routines`, once = randomUUID(), cron = randomUUID();
  const oncePath = `/api/projects/${f.project}/routines/${once}`, cronPath = `/api/projects/${f.project}/routines/${cron}`;
  const schedule = { kind: 'once', date: '2024-03-10', time: '02:30' };
  const command = { requestId: once, id: once, revision: 0, name: 'Once', prompt: 'PRIVATE_WALL_CLOCK_PROMPT', schedule, enabled: true };
  try {
    for (const extra of [{ nextAt: 0 }, { intervalSeconds: null }, { nextAt: 0, intervalSeconds: 300 }]) {
      assert.equal((await f.send(list, { ...command, ...extra })).status, 400);
    }
    globalThis.fetch = async (url, init) => {
      const response = await originalFetch(url, init);
      if (String(url) === f.origin() + list) {
        assert.equal(response.status, 201, await response.clone().text());
        await response.body?.cancel(); throw new Error('Lost wall-clock response');
      }
      return response;
    };
    await assert.rejects(f.send(list, command), /Lost wall-clock response/); globalThis.fetch = originalFetch;
    const saved = await f.send(list, command); assert.equal(saved.status, 200); assert.deepEqual(saved.body.data.schedule, schedule);
    assert.equal(saved.body.data.next_at, onceCandidate(schedule, f.home.zone).at);
    const cronCommand = { ...command, requestId: cron, id: cron, name: 'Daily cron', schedule: { kind: 'cron', expression: '0 9 * * *' } };
    assert.equal((await f.send(list, cronCommand)).status, 201);
    // Simulate a durable due time retained while the canonical process was down.
    db.prepare('UPDATE routines SET next_at=? WHERE id=?').run(onceCandidate({ date: '2024-01-01', time: '09:00' }, f.home.zone).at, cron);
    let pendingOnce: any, pendingCron: any;
    await until(async () => {
      pendingOnce = (await f.send(oncePath + '/runs')).body.data.items[0];
      pendingCron = (await f.send(cronPath + '/runs')).body.data.items[0];
      return pendingOnce?.state === 'pending' && pendingCron?.state === 'pending';
    });
    assert.equal(pendingOnce.scheduleKind, 'once'); assert.equal(pendingCron.scheduleKind, 'cron'); assert(pendingCron.missedCount > 1);
    assert.equal(pendingCron.id, createHash('sha256').update(JSON.stringify([cron, 1, f.home.zone_revision, pendingCron.dueAt, 0])).digest('base64url'));
    const response = await fetch(f.origin() + list + '/calendar.ics', { headers: { Authorization: `Bearer ${f.jwt()}`, Origin: f.origin(), Connection: 'close', 'Content-Type': 'application/json', 'X-Minarbot-Protocol': '1', 'X-Minarbot-Request-Id': randomUUID() } });
    const calendar = (await response.text()).replace(/\r\n /g, ''); assert.equal(response.status, 200);
    assert(calendar.includes(`UID:${once}@minarbot.local`)); assert(!calendar.includes(`UID:${cron}@minarbot.local`));
    assert(calendar.includes('Omitted cron routine "Daily cron"')); assert(!calendar.includes(command.prompt));
    const snapshot = (await f.send('/api/sync?projectId=' + f.project)).body.data;
    const observed = snapshot.page.events.filter((event: any) => event.recordKind.startsWith('routine'));
    assert(observed.every((event: any) => ['once', 'cron'].includes(event.body.scheduleKind))); assert(!JSON.stringify(observed).includes('PRIVATE_'));
    await f.restart(); assert.deepEqual((await f.send(list, command)).body.receipt, saved.body.receipt);
    assert.deepEqual((await f.send(oncePath + '/runs')).body.data.items, [pendingOnce]);
    assert.deepEqual((await f.send(cronPath + '/runs')).body.data.items, [pendingCron]);
    const connection = f.runtime.connect(); await until(() => connection.state().phase === 'connected');
    let admitted: any[] = [];
    await until(async () => {
      admitted = [(await f.send(oncePath + '/runs')).body.data.items[0], (await f.send(cronPath + '/runs')).body.data.items[0]];
      return admitted.every(item => item?.state === 'admitted');
    });
    assert.equal(admitted[0].id, pendingOnce.id); assert.equal(admitted[1].id, pendingCron.id);
    assert.equal((await f.send(oncePath)).body.data.enabled, 0); assert((await f.send(cronPath)).body.data.next_at > Date.now());
    const enable = randomUUID(), rejected = await f.send(oncePath + '/edit', { ...command, requestId: enable, id: enable, revision: 2 });
    assert.equal(rejected.status, 409); assert.equal(rejected.body.error.code, 'schedule_processed');
    await f.restart(); assert.deepEqual((await f.send(list, command)).body.receipt, saved.body.receipt);
    assert.equal((await f.send(oncePath + '/runs')).body.data.items[0].taskId, admitted[0].taskId);
    assert.equal((await f.send(cronPath + '/runs')).body.data.items[0].taskId, admitted[1].taskId);
    assert.equal(db.prepare('SELECT count(*) n FROM routine_occurrences').get()!.n, 2); assert.equal(db.prepare('SELECT count(*) n FROM tasks').get()!.n, 2);
    assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(), []);
  } finally { globalThis.fetch = originalFetch; db.close(); await f.close(); }
});

test('canonical routine calendar exports only current project and reader schedules without prompts',async()=>{
  const f=await rustWorkflowConfigurationFixture(),list=`/api/projects/${f.project}/agents/${f.agent}/routines`,path=list+'/calendar.ics',db=fixtureDatabase(join(f.dir,'canonical.sqlite'));
  const at=Date.parse('2027-01-01T09:00:00Z'),entries:Record<string,string>={};
  const calendar=async(subject='user_owner',suffix='',extra:Record<string,string>={})=>{
    const res=await fetch(f.origin()+path+suffix,{headers:{Authorization:`Bearer ${f.jwt(subject)}`,Origin:f.origin(),Connection:'close','Content-Type':'application/json','X-Minarbot-Protocol':'1','X-Minarbot-Request-Id':randomUUID(),...extra}});
    return {res,text:await res.text()};
  };
  try{
    const owner=(await f.send('/api/session')).body.data.accountId,collaborator=(await f.send('/api/session',undefined,'user_collaborator')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(f.project,collaborator);
    for(const name of ['shared','private','disabled','webhook','foreign']){
      const id=randomUUID();entries[name]=id;
      assert.equal((await f.send(list,{requestId:id,id,revision:0,name,prompt:'CALENDAR_PRIVATE_PROMPT',nextAt:at,intervalSeconds:name==='shared'?300:null,enabled:name!=='disabled',trigger:name==='webhook'?'webhook':'schedule'})).status,201);
    }
    db.prepare("UPDATE record_scopes SET visibility='owner' WHERE table_name='routines' AND key_json=?").run(JSON.stringify([entries.private]));
    db.prepare("UPDATE record_scopes SET project=? WHERE table_name='routines' AND key_json=?").run(f.other,JSON.stringify([entries.foreign]));
    const owned=await calendar();assert.equal(owned.res.status,200);assert.equal(owned.res.headers.get('content-type'),'text/calendar; charset=utf-8');
    assert.equal(owned.res.headers.get('x-minarbot-protocol'),'1');assert.equal(owned.res.headers.get('cache-control'),'no-store');
    assert(owned.text.includes(`UID:${entries.shared}@minarbot.local`));assert(owned.text.includes(`UID:${entries.private}@minarbot.local`));
    for(const name of ['disabled','webhook','foreign'])assert(!owned.text.includes(entries[name]),name);
    assert(!owned.text.includes('CALENDAR_PRIVATE_PROMPT'));assert(owned.text.includes('DTSTART:20270101T090000Z'));assert(owned.text.includes('RRULE:FREQ=SECONDLY;INTERVAL=300'));
    assert.equal((await calendar('user_collaborator')).res.status,404);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(f.project,collaborator,f.agent);
    db.prepare('INSERT INTO response_policies VALUES (?,?,?,0,1)').run(f.project,collaborator,f.agent);
    const shared=await calendar('user_collaborator');assert.equal(shared.res.status,200);assert(shared.text.includes(entries.shared));assert(!shared.text.includes(entries.private));
    db.prepare("UPDATE devices SET zone='Pacific/Honolulu',zone_revision=zone_revision+1 WHERE id=?").run(f.home.id);
    await f.restart();assert((await calendar('user_collaborator')).text.includes('DTSTART:20270101T090000Z'),'Viewer/home-zone changes cannot rewrite absolute timing');
    for(const suffix of ['?projectId='+f.project,'?after='+entries.shared])assert.equal((await calendar('user_collaborator',suffix)).res.status,404);
    assert.equal((await calendar('user_collaborator','',{'X-Minarbot-Project-Id':f.project})).res.status,404);
    db.prepare('UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?').run(f.project,collaborator);
    const denied=await calendar('user_collaborator');assert.equal(denied.res.status,404);assert(!denied.text.includes(entries.shared));
    assert.equal((await f.send(`/api/agents/${f.agent}/routines/calendar.ics`)).status,404);
    assert.equal(db.prepare('SELECT enabled FROM routines WHERE id=?').get(entries.shared)!.enabled,1,'Export cannot mutate a schedule');
  }finally{db.close();await f.close();}
});

test('canonical workflow configuration stays project scoped with immutable versions and retry receipts', async () => {
  const f = await rustWorkflowConfigurationFixture(), { home, project, other, workflow, graph, list, detail } = f;
  try {
    const command = { requestId: workflow, id: workflow, revision: 0, name: 'Original', graph };
    const created = await f.send(list, command); assert.equal(created.status, 201, JSON.stringify(created.body));
    assert.equal(created.body.data.projectId, project); assert.equal(created.body.data.homeDeviceId, home.id);
    const replay = await f.send(list, command); assert.equal(replay.status, 200); assert.deepEqual(replay.body.receipt, created.body.receipt);
    assert.deepEqual((await f.send(detail)).body.data.graph, graph);
    assert.deepEqual((await f.send(`/api/projects/${other}/workflows`)).body.data.items, []);
    assert.equal((await f.send(`/api/projects/${other}/workflows/${workflow}`)).status, 404);
    assert.equal((await f.send(`/api/projects/${other}/agents/${f.agent}`, { requestId: randomUUID(), expectedProjectRevision: 1, expectedAgentRevision: 1, expectedAuthorityRevision: 1, expectedParticipationRevision: 0, rules: 'Separate', capabilities: { v: 1, tools: [], resources: [] } }, 'user_owner', 'PUT')).status, 201);
    const otherWorkflow = randomUUID();
    assert.equal((await f.send(`/api/projects/${other}/agents/${f.agent}/workflows`, { ...command, requestId: otherWorkflow, id: otherWorkflow, name: 'Other project' })).status, 201);
    assert.deepEqual((await f.send(`/api/projects/${other}/workflows`)).body.data.items.map((row:{id:string}) => row.id), [otherWorkflow]);
    assert.equal((await f.send(`/api/projects/${other}/workflows/${workflow}`)).status, 404, 'Same agent participation cannot transfer workflows');
    assert.equal((await f.send(`/api/workflows/${workflow}`)).status, 404);
    for (const path of ['/api/workflows?deleted=1', `/api/agents/${f.agent}/workflows?after=${workflow}`, `/api/workflows/${workflow}/versions?before=2`, detail + '/run?before=2']) assert.equal((await f.send(path)).status, 404, path);
    const edited = { requestId: randomUUID(), revision: 1, name: 'Edited', graph: { ...graph, maxSteps: 2 } };
    const saved = await f.send(detail + '/edit', { ...edited, id: edited.requestId }); assert.equal(saved.status, 200, JSON.stringify(saved.body));
    await f.restart();
    assert.deepEqual((await f.send(detail + '/edit', { ...edited, id: edited.requestId })).body.receipt, saved.body.receipt);
    const versions = (await f.send(detail + '/versions')).body.data.items;
    assert.deepEqual(versions.map((row: {revision:number}) => row.revision), [2, 1]); assert.deepEqual(versions[1].graph, graph);
    const restore = { requestId: randomUUID(), revision: 2, version: 1 };
    assert.equal((await f.send(detail + '/restore', { ...restore, id: restore.requestId })).body.data.revision, 3);
    const deletion = { requestId: randomUUID(), revision: 3 };
    const deleted = await f.send(detail + '/delete', { ...deletion, id: deletion.requestId }); assert.equal(deleted.status, 200);
    assert.deepEqual((await f.send(detail + '/delete', { ...deletion, id: deletion.requestId })).body.receipt, deleted.body.receipt);
    assert.deepEqual((await f.send(`/api/projects/${project}/workflows`)).body.data.items, []);
    assert.equal((await f.send(`/api/projects/${project}/workflows?deleted=1`)).body.data.items[0].id, workflow);
    const runId = randomUUID(); assert.equal((await f.send(detail + '/run', { requestId: runId, id: runId, revision: 3, input: 'Must not execute' })).status, 404);
  } finally { await f.close(); }
});

test('canonical workflows enforce current assignments private mappings selectors and atomic receipt storage', async () => {
  const f = await rustWorkflowConfigurationFixture(), { project, other, agent, workflow, graph, list, detail } = f;
  const command = { requestId: workflow, id: workflow, revision: 0, name: 'Shared', graph };
  const db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
  try {
    const accepted = await f.send(list, command); assert.equal(accepted.status, 201);
    const collaborator = (await f.send('/api/session', undefined, 'user_collaborator')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(project, collaborator);
    assert.equal((await f.send(detail, undefined, 'user_collaborator')).status, 404);
    assert.deepEqual((await f.send(`/api/projects/${project}/workflows`, undefined, 'user_collaborator')).body.data.items, []);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(project, collaborator, agent);
    db.prepare('INSERT INTO response_policies VALUES (?,?,?,0,1)').run(project, collaborator, agent);
    assert.equal((await f.send(detail, undefined, 'user_collaborator')).status, 200, 'Response OFF does not remove workflow assignment');
    const edit = { requestId: randomUUID(), revision: 1, name: 'Collaborator edit', graph };
    const edited = await f.send(detail + '/edit', { ...edit, id: edit.requestId }, 'user_collaborator'); assert.equal(edited.status, 200);
    db.prepare('UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=? AND agent=?').run(project, collaborator, agent);
    assert.equal((await f.send(detail + '/edit', { ...edit, id: edit.requestId }, 'user_collaborator')).status, 404, 'Revoked assignment cannot replay a receipt');
    db.prepare('UPDATE collaborator_agents SET enabled=1,revision=revision+1 WHERE project=? AND account=? AND agent=?').run(project, collaborator, agent);
    const owner = (await f.send('/api/session')).body.data.accountId;
    db.prepare("UPDATE record_scopes SET visibility='owner' WHERE table_name='workflows' AND key_json=?").run(JSON.stringify([workflow]));
    assert.equal((await f.send(detail, undefined, 'user_collaborator')).status, 404);
    db.prepare("UPDATE record_scopes SET visibility='project' WHERE table_name='workflows' AND key_json=?").run(JSON.stringify([workflow]));
    db.prepare("UPDATE record_scopes SET project=? WHERE table_name='workflow_versions' AND key_json=?").run(other, JSON.stringify([workflow, 2]));
    assert.equal((await f.send(detail)).status, 404, 'Unmapped version does not return graph');
    db.prepare("UPDATE record_scopes SET project=? WHERE table_name='workflow_versions' AND key_json=?").run(project, JSON.stringify([workflow, 2]));
    db.prepare("UPDATE record_scopes SET project=? WHERE table_name='workflow_versions' AND key_json=?").run(other, JSON.stringify([workflow, 1]));
    assert.equal((await f.send(list, command)).status, 404, 'Receipt cannot return a graph whose version scope was removed');
    db.prepare("UPDATE record_scopes SET project=? WHERE table_name='workflow_versions' AND key_json=?").run(project, JSON.stringify([workflow, 1]));
    for (const query of ['projectId=' + project, 'after=x', 'after=', 'deleted=x', 'deleted=0&deleted=0']) {
      const path = query === 'after=' ? detail : `/api/projects/${project}/workflows`;
      const denied = await f.send(path + '?' + query); assert.equal(denied.status, 404, JSON.stringify(denied.body));
    }
    const headerResponse = await fetch(f.origin() + detail, { headers: { Authorization: 'Bearer ' + f.jwt(), 'Content-Type': 'application/json', 'X-Minarbot-Protocol': '1', 'X-Minarbot-Request-Id': randomUUID(), 'X-Minarbot-Project-Id': project } });
    assert.equal(headerResponse.status, 404); await headerResponse.body?.cancel();
    assert.equal((await f.send(detail + '/edit', { ...edit, id: edit.requestId, projectId: project })).status, 404);
    assert.equal((await f.send(detail + '/edit', { ...edit, requestId: randomUUID(), id: randomUUID() })).status, 400);
    assert.equal((await f.send(list, { ...command, name: 'Different' })).status, 409);
    assert.equal((await f.send(list, { ...command, name: ' Shared ' })).status, 200, 'Digest uses normalized feature name');
    const bad = randomUUID(), foreignGraph = { entry: 'work', maxSteps: 2, nodes: [{ id: 'work', kind: 'agent', agent: bad, prompt: 'foreign', next: 'out' }, ...graph.nodes] };
    assert.equal((await f.send(list, { ...command, id: bad, requestId: bad, graph: foreignGraph })).status, 400);
    const teammate = randomUUID();
    assert.equal((await f.send('/api/agents', { requestId: teammate, id: teammate, name: 'Other agent', model: 'fixture', backupModel: '', instructions: 'Help', threadLimit: 2, projectId: project, homeDeviceId: f.home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: f.home.revision, providerBinding: null })).status, 201);
    assert.equal((await f.send(list, { ...command, id: bad, requestId: bad, graph: { ...foreignGraph, nodes: [{ ...foreignGraph.nodes[0], agent: teammate }, ...graph.nodes] } }, 'user_collaborator')).status, 404, 'Every graph agent needs current assignment');
    const channel = randomUUID();
    db.prepare("INSERT INTO channels(id,name,instructions,lead,revision,archived) VALUES (?,?,'',?,1,0)").run(channel, 'Other project', agent); db.prepare('INSERT INTO channel_members VALUES (?,?)').run(channel, agent);
    db.prepare("INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)").run(JSON.stringify([channel]), owner, other, channel);
    assert.equal((await f.send(list, { ...command, id: bad, requestId: bad, graph: { ...graph, nodes: [{ id: 'out', kind: 'output', channel }] } })).status, 404);
    const change = { requestId: randomUUID(), revision: 2, name: 'Atomic', graph };
    db.exec("CREATE TRIGGER fail_workflow_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'retained:%/workflows/%/edit' BEGIN SELECT RAISE(ABORT,'workflow receipt failure'); END;");
    assert.equal((await f.send(detail + '/edit', { ...change, id: change.requestId })).status, 500);
    assert.equal((await f.send(detail)).body.data.name, 'Collaborator edit');
    assert.deepEqual((await f.send(detail + '/versions')).body.data.items.map((row: {revision:number}) => row.revision), [2,1]);
    db.exec('DROP TRIGGER fail_workflow_receipt');
    assert.equal((await f.send(detail + '/edit', { ...change, id: change.requestId })).status, 200);
    assert.equal(db.prepare('SELECT count(*) n FROM receipts WHERE request_id=?').get(change.requestId)!.n, 1);
    db.exec("CREATE TRIGGER fail_workflow_mapping BEFORE INSERT ON record_scopes WHEN NEW.table_name='workflow_versions' BEGIN SELECT RAISE(ABORT,'workflow mapping failure'); END;");
    assert.equal((await f.send(list, { ...command, requestId: bad, id: bad })).status, 500);
    assert.equal((await f.send(`/api/projects/${project}/workflows/${bad}`)).status, 404);
    assert.equal((await f.send(`/api/projects/${project}/workflows`)).body.data.items.length, 1, 'Failed mapping leaves no partial workflow');
    db.exec('DROP TRIGGER fail_workflow_mapping');
    assert.equal((await f.send(list, { ...command, requestId: bad, id: bad })).status, 201);
    assert.equal(db.prepare("SELECT count(*) n FROM sqlite_schema WHERE type='table' AND name IN('workflow_webhooks','workflow_schedules')").get()!.n, 0, 'Canonical startup never installs legacy schedule tables');
    assert(!db.prepare('PRAGMA table_info(workflow_waits)').all().some(row=>row.name==='secret'),'Canonical waits contain protected references only');
    db.prepare('UPDATE project_members SET deleted=1,grant_revision=grant_revision+1 WHERE project=? AND account=?').run(project, collaborator);
    assert.equal((await f.send(detail, undefined, 'user_collaborator')).status, 404);
    assert.equal((await f.send(detail, undefined, 'user_stranger')).status, 404);
    assert.equal(db.prepare('SELECT account FROM projects WHERE id=?').get(project)!.account, owner);
  } finally { db.close(); await f.close(); }
});

test('canonical workflow pages bound graph bytes and preserve list and version cursors', async () => {
  const f = await rustWorkflowConfigurationFixture(), { project, agent, workflow, graph, list, detail } = f;
  try {
    for (const id of [workflow, ...Array.from({ length: 22 }, () => randomUUID())]) assert.equal((await f.send(list, { requestId: id, id, revision: 0, name: 'Paged', graph })).status, 201);
    const first = (await f.send(`/api/projects/${project}/workflows`)).body.data; assert.equal(first.items.length, 20); assert(first.nextCursor);
    const second = (await f.send(`/api/projects/${project}/workflows?after=${first.nextCursor}`)).body.data;
    assert.equal(second.items.length, 3); assert.equal(second.nextCursor, null);
    assert.equal(new Set([...first.items, ...second.items].map(row => row.id)).size, 23);
    const large = { entry: 'work', maxSteps: 2, nodes: [{ id: 'work', kind: 'agent', agent, prompt: '🦉'.repeat(7000), next: 'out' }, ...graph.nodes] };
    for (let revision = 1; revision <= 10; revision++) { const id = randomUUID(); assert.equal((await f.send(detail + '/edit', { requestId: id, id, revision, name: 'Large versions', graph: large })).status, 200); }
    const versions: number[] = []; let before = '';
    do {
      const response = await f.send(detail + '/versions' + (before ? '?before=' + before : '')); assert.equal(response.status, 200);
      assert(Buffer.byteLength(JSON.stringify(response.body)) <= 256 * 1024);
      versions.push(...response.body.data.items.map((row: {revision:number}) => row.revision)); before = response.body.data.nextCursor ?? '';
    } while (before);
    assert.deepEqual(versions, [11,10,9,8,7,6,5,4,3,2,1]);
    for (const query of ['before=0','before=-1','before=abc','before=1&before=2','projectId=' + project]) assert.equal((await f.send(detail + '/versions?' + query)).status, 404);
  } finally { await f.close(); }
});

test('canonical workflow inbox keeps read state per authorized human across retry restart and deletion', async () => {
  const f = await rustWorkflowConfigurationFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
  try {
    const inbox = `/api/projects/${f.project}/workflow-notifications`, collaborator = (await f.send('/api/session', undefined, 'user_collaborator')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(f.project, collaborator);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(f.project, collaborator, f.agent);
    db.prepare('INSERT INTO response_policies VALUES (?,?,?,0,1)').run(f.project, collaborator, f.agent);
    const empty = await f.send(inbox); assert.equal(empty.status, 200); assert.deepEqual(empty.body.data.items, []);
    assert.equal((await f.send(f.list, { requestId: f.workflow, id: f.workflow, revision: 0, name: 'Reader scoped', graph: f.graph })).status, 201);
    const run = randomUUID(), runPath = `/api/projects/${f.project}/workflow-runs/${run}`;
    assert.equal((await f.send(f.detail + '/run', { requestId: run, id: run, revision: 1, input: 'Private result canary' })).status, 201);
    await until(async () => (await f.send(runPath)).body.data.status === 'done');
    for (const subject of ['user_owner','user_collaborator']) {
      const page = await f.send(inbox, undefined, subject); assert.equal(page.status, 200);
      assert.equal(page.body.data.unread, 1); assert.equal(page.body.data.items[0].run, run); assert.equal(page.body.data.items[0].read, 0);
      assert(!JSON.stringify(page.body).includes('Private result canary'));
    }
    const syncPath = '/api/sync?projectId=' + f.project, ownerCursor = (await f.send(syncPath)).body.data.nextCursor, memberCursor = (await f.send(syncPath,undefined,'user_collaborator')).body.data.nextCursor;
    const id = randomUUID(), command = { requestId: id, id }, readPath = inbox + '/' + run + '/read';
    db.exec(`CREATE TRIGGER fail_notice_receipt BEFORE INSERT ON receipts WHEN NEW.request_id='${id}' BEGIN SELECT RAISE(ABORT,'synthetic read receipt failure'); END`);
    assert.equal((await f.send(readPath,command)).status,500);
    assert.equal((await f.send(inbox)).body.data.unread,1);
    assert.deepEqual((await f.send(syncPath + '&cursor=' + encodeURIComponent(ownerCursor))).body.data.page.events,[]);
    db.exec('DROP TRIGGER fail_notice_receipt');
    const accepted = await f.send(readPath, command); assert.equal(accepted.status, 200);
    assert.deepEqual((await f.send(readPath, command)).body.receipt, accepted.body.receipt);
    assert.equal((await f.send(inbox)).body.data.unread, 0);
    assert.equal((await f.send(inbox, undefined, 'user_collaborator')).body.data.unread, 1);
    const ownerDelta = (await f.send(syncPath + '&cursor=' + encodeURIComponent(ownerCursor))).body.data;
    assert.deepEqual(ownerDelta.page.events.map((event: any) => [event.recordKind,event.recordId,event.revision,event.body.read]), [['workflow_notification',run,2,true]]);
    assert.deepEqual((await f.send(syncPath + '&cursor=' + encodeURIComponent(memberCursor),undefined,'user_collaborator')).body.data.page.events, []);
    const secondId = randomUUID(); assert.equal((await f.send(readPath,{requestId:secondId,id:secondId})).status,200);
    assert.deepEqual((await f.send(syncPath + '&cursor=' + encodeURIComponent(ownerDelta.nextCursor))).body.data.page.events, []);
    assert.equal((await f.send(readPath, command, 'user_collaborator')).status, 404);
    await f.restart();
    assert.deepEqual((await f.send(readPath, command)).body.receipt, accepted.body.receipt);
    const deleteId = randomUUID(); assert.equal((await f.send(f.detail + '/delete', { requestId: deleteId, id: deleteId, revision: 1 })).status, 200);
    const retained = (await f.send(inbox)).body.data; assert.equal(retained.items.length,1); assert.equal(retained.items[0].read, 1);
    assert.equal((await f.send(runPath)).body.data.last, 'Private result canary');
  } finally { db.close(); await f.close(); }
});

test('canonical workflow inbox hides foreign selectors and rechecks current reader scopes before receipt replay', async () => {
  const f = await rustWorkflowConfigurationFixture(), db = fixtureDatabase(join(f.dir,'canonical.sqlite'));
  try {
    const inbox = `/api/projects/${f.project}/workflow-notifications`, collaborator = (await f.send('/api/session',undefined,'user_collaborator')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(f.project,collaborator);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(f.project,collaborator,f.agent);
    assert.equal((await f.send(f.list,{requestId:f.workflow,id:f.workflow,revision:0,name:'Scoped inbox',graph:f.graph})).status,201);
    const run = randomUUID(), detail = `/api/projects/${f.project}/workflow-runs/${run}`, readPath = inbox + '/' + run + '/read';
    assert.equal((await f.send(f.detail+'/run',{requestId:run,id:run,revision:1,input:'Scoped result'})).status,201);
    await until(async () => (await f.send(detail)).body.data.status === 'done');
    const id = randomUUID(), command = {requestId:id,id};
    assert.equal((await f.send(readPath,command,'user_collaborator')).status,200);
    for (const path of ['/api/workflow-notifications','/api/workflow-notifications?before=2',inbox+'?projectId='+f.project,inbox+'?before=0',inbox+'?before=-1',inbox+'?before=1.5',inbox+'?before=9007199254740992',inbox+'?before=2&before=3',inbox+'/bad/read',readPath+'?before=2']) {
      const response = await f.send(path,path === readPath+'?before=2' ? command : undefined);
      assert.equal(response.status,404,path); assert.equal(response.body.error.code,'resource_not_found',path);
    }
    for (const path of [readPath.replace(f.project,f.other),readPath.replace(run,randomUUID())]) assert.equal((await f.send(path,command)).status,404);
    assert.equal((await f.send(inbox,undefined,'user_stranger')).status,404);
    assert.deepEqual((await f.send(inbox.replace(f.project,f.other))).body.data.items,[]);
    assert.equal((await f.send(readPath,{...command,projectId:f.project})).status,404);
    assert.equal((await f.send(readPath,{...command,read:true})).status,400);
    assert.equal((await f.send(readPath,{...command,id:randomUUID()})).status,400);
    for (const header of ['X-Minarbot-Project','X-Minarbot-Project-Id']) {
      const response = await fetch(f.origin()+inbox,{headers:{Authorization:`Bearer ${f.jwt()}`,Origin:f.origin(),'Content-Type':'application/json','X-Minarbot-Protocol':'1','X-Minarbot-Request-Id':randomUUID(),[header]:f.project}});
      assert.equal(response.status,404); await response.arrayBuffer();
    }
    for (const [table,key] of [['workflows',[f.workflow]],['workflow_versions',[f.workflow,1]],['workflow_runs',[run]]] as const) {
      const syncPath='/api/sync?projectId='+f.project, before=(await f.send(syncPath,undefined,'user_collaborator')).body.data;
      db.prepare("UPDATE record_scopes SET visibility='owner' WHERE table_name=? AND key_json=?").run(table,canonicalJson(key));
      const hidden = (await f.send(inbox,undefined,'user_collaborator')).body.data; assert.deepEqual(hidden.items,[]); assert.equal(hidden.unread,0);
      const denied = await f.send(readPath,command,'user_collaborator'); assert.equal(denied.status,404); assert.equal(denied.body.receipt,undefined);
      assert.equal((await f.send(syncPath+'&cursor='+encodeURIComponent(before.nextCursor),undefined,'user_collaborator')).status,410);
      assert(!(await f.send(syncPath,undefined,'user_collaborator')).body.data.page.events.some((event:any)=>event.recordKind==='workflow_notification'));
      db.prepare("UPDATE record_scopes SET visibility='project' WHERE table_name=? AND key_json=?").run(table,canonicalJson(key));
      assert.equal((await f.send(readPath,command,'user_collaborator')).status,200);
    }
    db.prepare("UPDATE record_scopes SET visibility='owner',account=? WHERE table_name='workflow_runs' AND key_json=?").run(collaborator,canonicalJson([run]));
    assert.deepEqual((await f.send(inbox)).body.data.items,[],'Owner cannot see another human private result notice');
    assert.equal((await f.send(inbox,undefined,'user_collaborator')).body.data.items.length,1);
    db.prepare("UPDATE record_scopes SET visibility='project' WHERE table_name='workflow_runs' AND key_json=?").run(canonicalJson([run]));
    db.prepare('UPDATE agent_projects SET deleted=1 WHERE project=? AND agent=?').run(f.project,f.agent);
    assert.deepEqual((await f.send(inbox)).body.data.items,[]); assert.equal((await f.send(readPath,command,'user_collaborator')).status,404);
    db.prepare('UPDATE agent_projects SET deleted=0 WHERE project=? AND agent=?').run(f.project,f.agent);
    db.prepare('UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=? AND agent=?').run(f.project,collaborator,f.agent);
    assert.deepEqual((await f.send(inbox,undefined,'user_collaborator')).body.data.items,[]); assert.equal((await f.send(readPath,command,'user_collaborator')).status,404);
    db.prepare('UPDATE project_members SET deleted=1 WHERE project=? AND account=?').run(f.project,collaborator);
    assert.equal((await f.send(inbox,undefined,'user_collaborator')).status,404);
  } finally { db.close(); await f.close(); }
});

test('canonical workflow inbox pages twenty outputs without duplicate completion or recipient backfill', async () => {
  const f = await rustWorkflowConfigurationFixture(), db = fixtureDatabase(join(f.dir,'canonical.sqlite'));
  try {
    const inbox = `/api/projects/${f.project}/workflow-notifications`, collaborator = (await f.send('/api/session',undefined,'user_collaborator')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(f.project,collaborator);
    assert.equal((await f.send(f.list,{requestId:f.workflow,id:f.workflow,revision:0,name:'Paged inbox',graph:f.graph})).status,201);
    const runs: string[] = [];
    for (let n=0;n<21;n++) {
      const run=randomUUID(), command={requestId:run,id:run,revision:1,input:String(n)}; runs.push(run);
      const accepted=await f.send(f.detail+'/run',command); assert.equal(accepted.status,201);
      assert.deepEqual((await f.send(f.detail+'/run',command)).body.receipt,accepted.body.receipt);
    }
    await until(async () => (await f.send(inbox)).body.data.unread === 21);
    const first=(await f.send(inbox)).body.data; assert.equal(first.items.length,20); assert(first.nextCursor);
    const second=(await f.send(inbox+'?before='+first.nextCursor)).body.data; assert.equal(second.items.length,1); assert.equal(second.nextCursor,null); assert.equal(second.unread,21);
    assert.deepEqual([...first.items,...second.items].map((item:any)=>item.run).sort(),runs.sort());
    assert.deepEqual((await f.send(inbox,undefined,'user_collaborator')).body.data.items,[]);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(f.project,collaborator,f.agent);
    assert.equal((await f.send(`/api/projects/${f.project}/workflow-runs/${runs[0]}`,undefined,'user_collaborator')).status,200);
    assert.deepEqual((await f.send(inbox,undefined,'user_collaborator')).body.data.items,[]);
    await f.restart(); assert.equal((await f.send(inbox)).body.data.unread,21);
    assert.deepEqual((await f.send(inbox+'?before='+first.nextCursor)).body.data,second);
    const last=randomUUID(); assert.equal((await f.send(f.detail+'/run',{requestId:last,id:last,revision:1,input:'Newly assigned reader'})).status,201);
    await until(async ()=>(await f.send(inbox,undefined,'user_collaborator')).body.data.unread===1);
    assert.equal((await f.send(inbox,undefined,'user_collaborator')).body.data.items[0].run,last);
    const read=randomUUID(), command={requestId:read,id:read};
    assert.equal((await f.send(inbox+'/'+runs[0]+'/read',command)).status,200);
    assert.equal((await f.send(inbox+'/'+last+'/read',command)).status,409);
  } finally { db.close(); await f.close(); }
});

test('workflow sync publishes metadata and revisioned run transitions once across retry and restart', async () => {
  const f = await rustWorkflowConfigurationFixture();
  try {
    const syncPath = '/api/sync?projectId=' + f.project, before = (await f.send(syncPath)).body.data;
    const graph = { entry: 'approval', maxSteps: 2, nodes: [{ id: 'approval', kind: 'approval', prompt: 'Private approval canary', yes: 'out' }, { id: 'out', kind: 'output' }] };
    const create = { requestId: f.workflow, id: f.workflow, revision: 0, name: 'Observed workflow', graph };
    const saved = await f.send(f.list, create); assert.equal(saved.status, 201);
    assert.deepEqual((await f.send(f.list, create)).body.receipt, saved.body.receipt);
    const created = (await f.send(syncPath + '&cursor=' + encodeURIComponent(before.nextCursor))).body.data;
    assert.deepEqual(created.page.events.map((event: any) => [event.recordKind,event.recordId,event.revision]), [['workflow',f.workflow,1]]);
    assert.equal(created.page.events[0].body.detailRef, f.detail);
    const run = randomUUID(), path = `/api/projects/${f.project}/workflow-runs/${run}`, command = { requestId: run, id: run, revision: 1, input: 'Private workflow input canary' };
    const accepted = await f.send(f.detail + '/run', command); assert.equal(accepted.status, 201);
    assert.deepEqual((await f.send(f.detail + '/run', command)).body.receipt, accepted.body.receipt);
    await until(async () => (await f.send(path)).body.data.status === 'approval');
    const approveId = randomUUID(), approve = { requestId: approveId, id: approveId, revision: 2, accepted: true };
    assert.equal((await f.send(path + '/approve', approve)).status, 200);
    await until(async () => (await f.send(path)).body.data.status === 'done');
    await f.restart();
    assert.equal((await f.send(path + '/approve', approve)).status, 200);
    const delta = (await f.send(syncPath + '&cursor=' + encodeURIComponent(created.nextCursor))).body.data;
    assert.deepEqual(delta.page.events.map((event: any) => [event.recordKind,event.revision,event.body.status]), [['workflow_run',1,'ready'],['workflow_run',2,'approval'],['workflow_run',3,'ready'],['workflow_notification',1,undefined],['workflow_run',4,'done']]);
    for (const event of delta.page.events) { assert.equal(event.recordId, run); assert.equal(event.body.detailRef, path); }
    const snapshot = (await f.send(syncPath)).body.data;
    assert.equal(snapshot.page.events.find((event: any) => event.recordKind === 'workflow_run').revision, 4);
    assert(!JSON.stringify([created,delta,snapshot]).includes('canary'));
    assert(!JSON.stringify(delta).includes('graph'));
  } finally { await f.close(); }
});

test('workflow sync includes protected wait activation delivery and expiry without secret references', async () => {
  const f = await protectedWorkflowWaitFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
  try {
    const syncPath = '/api/sync?projectId=' + f.project, snapshot = (await f.send(syncPath)).body.data;
    const initial = snapshot.page.events.find((event: any) => event.recordKind === 'workflow_run');
    assert.equal(initial.revision, 2); assert.equal(initial.body.status, 'waiting');
    const secret = [...f.keys.values].find(([key]) => key.includes('/event-wait/'))![1];
    const time = String(Math.floor(Date.now() / 1000)), delivery = randomUUID(), body = '{"event":"ready","payload":"Private event canary"}';
    const send = async () => {
      const response = await fetch(f.origin() + f.waitPath + '/events', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Minarbot-Time': time, 'X-Minarbot-Id': delivery, 'X-Minarbot-Signature': createHmac('sha256', secret).update(`${time}\n${delivery}\n${body}`).digest('hex') }, body });
      return { status: response.status, body: await response.json() };
    };
    db.exec("CREATE TRIGGER fail_workflow_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_run' BEGIN SELECT RAISE(ABORT,'synthetic workflow event failure'); END");
    assert.equal((await send()).status, 500);
    assert.equal((await f.send(f.waitPath)).body.data.status, 'waiting');
    assert.equal((await f.send(f.path)).body.data.revision, 2);
    db.exec('DROP TRIGGER fail_workflow_sync');
    const accepted = await send(); assert.equal(accepted.status, 200);
    await until(async () => (await f.send(f.path)).body.data.status === 'done');
    assert.deepEqual((await send()).body.receipt, accepted.body.receipt);
    const delta = (await f.send(syncPath + '&cursor=' + encodeURIComponent(snapshot.nextCursor))).body.data;
    assert.deepEqual(delta.page.events.map((event: any) => [event.recordKind,event.revision,event.body.status]), [['workflow_run',3,'ready'],['workflow_notification',1,undefined],['workflow_run',4,'done']]);
    const observations = JSON.stringify([snapshot,delta]);
    for (const value of [secret,'secret_ref','secretRef','secret_generation','Private event canary']) assert(!observations.includes(value));
    const editId = randomUUID(), graph = { entry: 'event', maxSteps: 2, nodes: [{ id: 'event', kind: 'wait', event: 'ready', timeoutSeconds: 2, next: 'out' }, { id: 'out', kind: 'output' }] };
    assert.equal((await f.send(f.detail + '/edit', { requestId: editId, id: editId, revision: 1, name: 'Expiry', graph })).status, 200);
    const next = randomUUID(), path = `/api/projects/${f.project}/workflow-runs/${next}`;
    const before = (await f.send(syncPath)).body.data;
    assert.equal((await f.send(f.detail + '/run', { requestId: next, id: next, revision: 2, input: '' })).status, 201);
    await until(async () => (await f.send(path)).body.data.status === 'waiting');
    await until(async () => (await f.send(path)).body.data.status === 'failed');
    const expired = (await f.send(syncPath + '&cursor=' + encodeURIComponent(before.nextCursor))).body.data;
    assert.deepEqual(expired.page.events.map((event: any) => [event.revision,event.body.status]), [[1,'ready'],[2,'waiting'],[3,'failed']]);
  } finally { db.close(); await f.close(); }
});

test('workflow sync rolls back commands with event failures and retains deletion tombstones', async () => {
  const f = await rustWorkflowConfigurationFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
  try {
    const syncPath = '/api/sync?projectId=' + f.project, before = (await f.send(syncPath)).body.data;
    const graph = { entry: 'approval', maxSteps: 2, nodes: [{ id: 'approval', kind: 'approval', prompt: 'Approve', yes: 'out' }, { id: 'out', kind: 'output' }] };
    const create = { requestId: f.workflow, id: f.workflow, revision: 0, name: 'Original', graph };
    db.exec("CREATE TRIGGER fail_workflow_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow' BEGIN SELECT RAISE(ABORT,'synthetic workflow sync failure'); END");
    assert.equal((await f.send(f.list, create)).status, 500); assert.equal((await f.send(f.detail)).status, 404);
    assert.equal((await f.send(syncPath + '&cursor=' + encodeURIComponent(before.nextCursor))).body.data.page.events.length, 0);
    db.exec('DROP TRIGGER fail_workflow_sync');
    assert.equal((await f.send(f.list, create)).status, 201);
    for (const [operation,fields] of [['edit',{ name: 'Edited', graph }],['restore',{ version: 1 }]] as const) {
      const commandId = randomUUID(), revision = operation === 'edit' ? 1 : 2, command = { requestId: commandId, id: commandId, revision, ...fields };
      const accepted = await f.send(f.detail + '/' + operation, command); assert.equal(accepted.status, 200);
      assert.deepEqual((await f.send(f.detail + '/' + operation, command)).body.receipt, accepted.body.receipt);
    }
    const run = randomUUID(), path = `/api/projects/${f.project}/workflow-runs/${run}`, command = { requestId: run, id: run, revision: 3, input: '' };
    db.exec("CREATE TRIGGER fail_run_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_run' BEGIN SELECT RAISE(ABORT,'synthetic run sync failure'); END");
    assert.equal((await f.send(f.detail + '/run', command)).status, 500); assert.equal((await f.send(path)).status, 404);
    db.exec('DROP TRIGGER fail_run_sync');
    assert.equal((await f.send(f.detail + '/run', command)).status, 201);
    await until(async () => (await f.send(path)).body.data.status === 'approval');
    const cancelId = randomUUID(), cancellation = { requestId: cancelId, id: cancelId, revision: 2 };
    db.exec("CREATE TRIGGER fail_run_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_run' BEGIN SELECT RAISE(ABORT,'synthetic run sync failure'); END");
    assert.equal((await f.send(path + '/cancel', cancellation)).status, 500); assert.equal((await f.send(path)).body.data.status, 'approval');
    db.exec('DROP TRIGGER fail_run_sync');
    const cancelled = await f.send(path + '/cancel', cancellation); assert.equal(cancelled.status, 200);
    assert.deepEqual((await f.send(path + '/cancel', cancellation)).body.receipt, cancelled.body.receipt);
    const deleteId = randomUUID(), deletion = { requestId: deleteId, id: deleteId, revision: 3 };
    assert.equal((await f.send(f.detail + '/delete', deletion)).status, 200);
    assert.equal((await f.send(f.detail + '/delete', deletion)).status, 200);
    const delta = (await f.send(syncPath + '&cursor=' + encodeURIComponent(before.nextCursor))).body.data;
    assert.deepEqual(delta.page.events.map((event: any) => [event.recordKind,event.revision,event.operation]), [['workflow',1,'upsert'],['workflow',2,'upsert'],['workflow',3,'upsert'],['workflow_run',1,'upsert'],['workflow_run',2,'upsert'],['workflow_run',3,'upsert'],['workflow',4,'delete']]);
    const snapshot = (await f.send(syncPath)).body.data;
    assert.equal(snapshot.page.events.find((event: any) => event.recordKind === 'workflow').operation, 'delete');
    assert.equal(snapshot.page.events.find((event: any) => event.recordKind === 'workflow_run').body.status, 'cancelled');
  } finally { db.close(); await f.close(); }
});

test('workflow sync rechecks assignment project and private root version and run scopes', async () => {
  const f = await rustWorkflowConfigurationFixture(), db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
  try {
    const collaborator = (await f.send('/api/session', undefined, 'user_collaborator')).body.data.accountId;
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(f.project, collaborator);
    const graph = { entry: 'approval', maxSteps: 2, nodes: [{ id: 'approval', kind: 'approval', prompt: 'Approve', yes: 'out' }, { id: 'out', kind: 'output' }] };
    assert.equal((await f.send(f.list, { requestId: f.workflow, id: f.workflow, revision: 0, name: 'Scoped', graph })).status, 201);
    const run = randomUUID(), runPath = `/api/projects/${f.project}/workflow-runs/${run}`;
    assert.equal((await f.send(f.detail + '/run', { requestId: run, id: run, revision: 1, input: '' })).status, 201);
    await until(async () => (await f.send(runPath)).body.data.status === 'approval');
    const syncPath = '/api/sync?projectId=' + f.project;
    const read = async (subject = 'user_collaborator') => (await f.send(syncPath, undefined, subject)).body.data;
    const kinds = (data: any) => data.page.events.filter((event: any) => event.recordKind.startsWith('workflow')).map((event: any) => event.recordKind);
    assert.deepEqual(kinds(await read()), []);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(f.project, collaborator, f.agent);
    db.prepare('INSERT INTO response_policies VALUES (?,?,?,0,1)').run(f.project, collaborator, f.agent);
    const allowed = await read(); assert.deepEqual(kinds(allowed), ['workflow','workflow_run']);
    assert.deepEqual(kinds((await f.send('/api/sync?projectId=' + f.other)).body.data), []);
    for (const [table,key,expected] of [['workflows',[f.workflow],[]],['workflow_versions',[f.workflow,1],[]],['workflow_runs',[run],['workflow']]] as const) {
      const before = await read();
      db.prepare("UPDATE record_scopes SET visibility='owner' WHERE table_name=? AND key_json=?").run(table, canonicalJson(key));
      const denied = await f.send(syncPath + '&cursor=' + encodeURIComponent(before.nextCursor), undefined, 'user_collaborator');
      assert.equal(denied.status, 410); assert.equal(denied.body.error.code, 'resync_required');
      assert.deepEqual(kinds(await read()), expected);
      assert.equal((await f.send(table === 'workflow_runs' ? runPath : f.detail, undefined, 'user_collaborator')).status, 404);
      db.prepare("UPDATE record_scopes SET visibility='project' WHERE table_name=? AND key_json=?").run(table, canonicalJson(key));
    }
    db.prepare("UPDATE record_scopes SET visibility='owner',account=? WHERE table_name='workflows' AND key_json=?").run(collaborator, canonicalJson([f.workflow]));
    assert.deepEqual(kinds(await read('user_owner')), [], 'Project owner cannot read another human private root');
    assert.deepEqual(kinds(await read()), ['workflow','workflow_run']);
    db.prepare("UPDATE record_scopes SET visibility='project' WHERE table_name='workflows' AND key_json=?").run(canonicalJson([f.workflow]));
    const beforeRevoke = await read();
    db.prepare('UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=? AND agent=?').run(f.project, collaborator, f.agent);
    assert.equal((await f.send(syncPath + '&cursor=' + encodeURIComponent(beforeRevoke.nextCursor), undefined, 'user_collaborator')).status, 410);
    assert.deepEqual(kinds(await read()), []);
    assert.equal((await f.send(syncPath, undefined, 'user_stranger')).status, 403);
  } finally { db.close(); await f.close(); }
});

test('signed human sync preserves delta cursors through canonical restart and rejects foreign or revoked reads', async () => {
  const f = await fixture();
  try {
    const project = randomUUID(), create = { requestId: randomUUID(), id: project, name: 'Sync project' };
    assert.equal((await f.send('/api/projects', create)).status, 201);
    const path = '/api/sync?projectId=' + project, first = await f.send(path);
    assert.equal(first.status, 200); assert(first.body.data.nextCursor);
    const edit = { requestId: randomUUID(), expectedRevision: 1, name: 'Persisted sync value' };
    const saved = await f.send('/api/projects/' + project, edit, 'user_owner', 'PATCH'); assert.equal(saved.status, 200);
    assert.deepEqual((await f.send('/api/projects/' + project, edit, 'user_owner', 'PATCH')).body.receipt, saved.body.receipt);
    await f.restart();
    const deltaPath = path + '&cursor=' + encodeURIComponent(first.body.data.nextCursor), delta = await f.send(deltaPath);
    assert.equal(delta.status, 200); assert.equal(delta.body.data.page.events.length, 1); assert.equal(delta.body.data.page.events[0].body.name, 'Persisted sync value');
    assert.equal((await f.send(deltaPath, undefined, 'user_stranger')).status, 403);
    for (const suffix of ['&cursor=', '&page=broken', '&repo=private', '&projectId=' + project]) assert.equal((await f.send(path + suffix)).status, 400);
    const denied = await f.send('/api/projects/' + project, { requestId: randomUUID(), expectedRevision: 2, deleted: true }, 'user_owner', 'PATCH');
    assert.equal(denied.status, 200);
    const revoked = await f.send(deltaPath); assert.equal(revoked.status, 403); assert.equal(revoked.body.ok, false); assert.equal(revoked.body.data, undefined);
  } finally { await f.close(); }
});

async function loopTask(f: Awaited<ReturnType<typeof fixture>>, runtime: { deviceId(): string }, setup = false, existingBinding?: string | null) {
  const home = (await f.send('/api/devices')).body.data.find((row: { id: string }) => row.id === runtime.deviceId()), project = randomUUID(), agent = randomUUID(), binding = existingBinding === undefined ? randomUUID() : existingBinding;
  assert.equal((await f.send('/api/projects', { requestId: randomUUID(), id: project, name: 'Execution loop' })).status, 201);
  if (existingBinding === undefined) {
    const seed = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
    try { seed.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible',?,'Synthetic','ready',1)").run(binding, home.id, seed.prepare('SELECT account FROM devices WHERE id=?').get(home.id)!.account, binding); } finally { seed.close(); }
  }
  assert.equal((await f.send('/api/agents', { requestId: agent, id: agent, name: 'Loop agent', model: 'fixture', backupModel: '', instructions: 'Answer carefully', threadLimit: 2, projectId: project, homeDeviceId: home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: home.revision, providerBinding: binding ? { bindingId: binding, expectedRevision: 1 } : null, setup })).status, 201);
  const taskId = setup ? agent : randomUUID();
  if (!setup) {
    const payload = { v: 1, kind: 'direct', message: { text: 'Answer once', files: [] } }, expectedRevisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 };
    { const admitted = await f.send('/api/tasks', { requestId: taskId, projectId: project, agentId: agent, homeDeviceId: home.id, payload, expectedRevisions, payloadDigest: digestCommand(['minarbot-task-payload-v1', taskId, project, agent, home.id, payload, expectedRevisions]) }); assert.equal(admitted.status, 202, JSON.stringify(admitted.body)); }
  }
  return { taskId, agent, project, binding };
}

test('signed task and device mutations publish atomic scoped observations across retry and restart', async () => {
  const f = await fixture(), runtime = f.device(join(f.dir, 'sync-home.sqlite'));
  try {
    const project = randomUUID(); assert.equal((await f.send('/api/projects', { requestId: randomUUID(), id: project, name: 'Device sync' })).status, 201);
    const path = '/api/sync?projectId=' + project, first = (await f.send(path)).body.data;
    await runtime.enroll(f.jwt()); const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
    const home = (await f.send('/api/devices')).body.data[0];
    const deviceDelta = (await f.send(path + '&cursor=' + encodeURIComponent(first.nextCursor))).body.data;
    assert.deepEqual(deviceDelta.page.events.map((event: any) => [event.recordKind, event.body.status]), [['device','offline'],['device','connecting'],['device','ready']]);
    assert(!JSON.stringify(deviceDelta).includes('signingPublicKey')); assert(!JSON.stringify(deviceDelta).includes('deviceSession'));
    const agent = randomUUID();
    assert.equal((await f.send('/api/agents', { requestId: agent, id: agent, name: 'Sync agent', model: 'fixture', backupModel: '', instructions: 'Private profile canary', threadLimit: 2, projectId: project, homeDeviceId: home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: home.revision, providerBinding: null })).status, 201);
    const before = (await f.send(path)).body.data, taskId = randomUUID(), payload = { v: 1, kind: 'direct', message: { text: 'Private prompt canary', files: [] } }, expectedRevisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 };
    const command = { requestId: taskId, projectId: project, agentId: agent, homeDeviceId: home.id, payload, expectedRevisions, payloadDigest: digestCommand(['minarbot-task-payload-v1', taskId, project, agent, home.id, payload, expectedRevisions]) };
    const accepted = await f.send('/api/tasks', command); assert.equal(accepted.status, 202); assert.deepEqual((await f.send('/api/tasks', command)).body.receipt, accepted.body.receipt);
    assert.equal((await f.send(`/api/tasks/${taskId}/control`, { requestId: randomUUID(), expectedRevision: 1, operation: 'pause' })).status, 200);
    const cancellation = { requestId: randomUUID(), expectedRevision: 2, operation: 'cancel' }, inspect = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
    try {
      inspect.exec("CREATE TRIGGER fail_task_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='task' BEGIN SELECT RAISE(ABORT,'synthetic event failure'); END");
      assert.equal((await f.send(`/api/tasks/${taskId}/control`, cancellation)).status, 500);
      assert.equal((await f.send(`/api/tasks/${taskId}`)).body.data.state, 'paused');
      inspect.exec('DROP TRIGGER fail_task_sync');
    } finally { inspect.close(); }
    const cancelled = await f.send(`/api/tasks/${taskId}/control`, cancellation); assert.equal(cancelled.status, 200);
    assert.deepEqual((await f.send(`/api/tasks/${taskId}/control`, cancellation)).body.receipt, cancelled.body.receipt);
    const deltaPath = path + '&cursor=' + encodeURIComponent(before.nextCursor), delta = (await f.send(deltaPath)).body.data;
    assert.deepEqual(delta.page.events.map((event: any) => [event.recordId,event.revision,event.body.state]), [[taskId,1,'queued'],[taskId,2,'paused'],[taskId,3,'cancelled']]);
    assert(!JSON.stringify(delta).includes('canary')); assert(!JSON.stringify(delta).includes(command.payloadDigest));
    const completed = await loopTask(f, runtime), completedPath = '/api/sync?projectId=' + completed.project, queued = (await f.send(completedPath)).body.data;
    await runtime.runTask(completed.taskId, () => ({ text: 'Private result canary' }));
    const completionDelta = (await f.send(completedPath + '&cursor=' + encodeURIComponent(queued.nextCursor))).body.data;
    assert.equal(completionDelta.page.events.at(-1).body.state, 'completed'); assert(completionDelta.page.events.some((event: any) => event.body.state === 'running')); assert(!JSON.stringify(completionDelta).includes('Private result canary'));
    const failure = fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try {
      failure.exec("CREATE TRIGGER fail_device_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='device' BEGIN SELECT RAISE(ABORT,'synthetic offline event failure'); END");
      await connection.stop(); assert.equal((await f.send('/api/session')).status,200);
      failure.exec('DROP TRIGGER fail_device_sync');
    } finally { failure.close(); }
    // A failed disconnect write is retried within seconds (about 2 s here, 30 s before the retry), but
    // under release-suite load the disconnect itself can wait for the 30 s sweep, so allow a full cycle.
    await until(async () => (await f.send('/api/devices')).body.data[0].status === 'offline', deviceReconnectTimeout);
    await f.restart();
    await until(async () => { try { return (await f.send(deltaPath)).status === 200; } catch { return false; } });
    assert.deepEqual((await f.send(deltaPath)).body.data.page.events.filter((event: any) => event.recordKind === 'task'), delta.page.events);
    assert.equal((await f.send(deltaPath, undefined, 'user_stranger')).status, 403);
  } finally { await runtime.close(); await f.close(); }
});

async function rustTaskFixture(f: Awaited<ReturnType<typeof fixture>>, filename: string, server = f.origin()) {
  const database = join(f.dir, filename), keyring = database + '.keys.json';
  const options = { database, keyring, server };
  writeFileSync(keyring, '{}', { mode: 0o600 });
  const { deviceId } = await enrollRustDevice(options, f.jwt());
  return {
    deviceId: () => deviceId,
    runTask: (taskId: string, provide: (context: ActionContext) => unknown) =>
      withDeviceProvider(provide, origin => runRustDeviceTask(options, taskId, origin)),
    crashTask: (taskId: string, provide: (context: ActionContext) => unknown, checkpoint: string) =>
      withDeviceProvider(provide, origin => crashRustDeviceTask(options, taskId, origin, checkpoint)),
  };
}

test('home execution loop publishes one provider answer through signed canonical HTTP', async () => {
  const f = await fixture();
  try {
    const runtime = await rustTaskFixture(f, 'loop.sqlite');
    const { taskId } = await loopTask(f, runtime); let calls = 0;
    const provider = (context: ActionContext) => {
      calls++;
      assert.equal(context.snapshotRef?.digest, sha256(canonicalJson(context.snapshot)));
      for (const tool of context.snapshot.tools as { function: { name: string } }[]) assert(!['read_attachment','create_text_file','publish_site','delegate_task'].includes(tool.function.name));
      return { text: 'Canonical loop answer' };
    };
    assert.equal((await runtime.runTask(taskId, provider)).state, 'completed');
    assert.equal((await runtime.runTask(taskId, provider)).state, 'completed');
    const detail = (await f.send(`/api/tasks/${taskId}`)).body.data;
    assert.equal(detail.state, 'completed'); assert.deepEqual(detail.results.map((row: { content: unknown }) => row.content), [{ text: 'Canonical loop answer' }]);
    assert.equal(calls, 1);
  } finally { await f.close(); }
});

test('home execution loop saves setup tools, pauses for answers and continues without repeating saved provider output', async () => {
  const f = await fixture();
  try {
    const runtime = await rustTaskFixture(f, 'setup-loop.sqlite');
    const { taskId } = await loopTask(f, runtime, true); let calls = 0;
    const provider = (context: { snapshot: Record<string, unknown> }) => {
      calls++;
      if (calls === 1) return { toolCall: { id: 'question', function: { name: 'ask_user', arguments: JSON.stringify({ question: 'Research or writing?', choices: ['Research', 'Writing'] }) } } };
      assert.match(JSON.stringify(context.snapshot.conversation), /Research/);
      if (calls === 2) return { toolCall: { id: 'profile', function: { name: 'edit_profile', arguments: JSON.stringify({ revision: 1, name: 'Researcher', role: 'Research agent', instructions: 'Research carefully' }) } } };
      assert.equal((context.snapshot.profile as { name: string }).name, 'Loop agent');
      assert.match(JSON.stringify(context.snapshot.toolJournal), /Researcher/);
      return { text: 'Research profile saved' };
    };
    assert.equal((await runtime.runTask(taskId, provider)).state, 'paused'); assert.equal(calls, 1);
    const paused = (await f.send(`/api/tasks/${taskId}`)).body.data;
    assert.equal((await f.send(`/api/tasks/${taskId}/questions/${paused.questions[0].id}/answer`, { requestId: randomUUID(), expectedRevision: paused.revision, text: 'Research' })).status, 200);
    assert.equal((await runtime.runTask(taskId, provider)).state, 'completed'); assert.equal(calls, 3);
    const detail = (await f.send(`/api/tasks/${taskId}`)).body.data; assert.equal(detail.results.length, 5);
    assert.deepEqual(detail.results[3].content, { name: 'Researcher', role: 'Research agent', instructions: 'Research carefully', revision: 2 });
  } finally { await f.close(); }
});

test('home execution loop rejects malformed or unadvertised provider outputs and never retries uncertain effects', async () => {
  const f = await fixture();
  try {
    const runtime = await rustTaskFixture(f, 'invalid-loop.sqlite');
    const call = { id: 'bad', function: { name: 'not_advertised', arguments: '{}' } };
    const outputs = [{ text: '' }, { text: 'Mixed', toolCall: call }, { toolCall: call }, { toolCall: { ...call, function: { name: 'ask_user', arguments: 'not json' } } }, { unexpected: 'bad' }];
    for (const [index, output] of outputs.entries()) {
      if (index !== 0 && index % 2 === 0) {
        await until(async () => (await f.send('/api/devices')).body.data[0].status !== 'ready', deviceReconnectTimeout);
        await f.restart();
      }
      const { taskId } = await loopTask(f, runtime); let calls = 0;
      const provider = () => { calls++; return output; };
      await assert.rejects(runtime.runTask(taskId, provider), (error: unknown) => error instanceof ProtocolError && error.code === 'provider_result_invalid');
      assert.equal((await runtime.runTask(taskId, provider)).state, 'uncertain'); assert.equal(calls, 1);
      const detail = (await f.send(`/api/tasks/${taskId}`)).body.data; assert.equal(detail.state, 'uncertain'); assert.deepEqual(detail.results, []);
    }
  } finally { await f.close(); }
});

test('home execution loop resumes saved provider output after completion reply loss without another provider call', async () => {
  const f = await fixture(), filename = 'loop-restart.sqlite';
  try {
    const runtime = await rustTaskFixture(f, filename);
    const { taskId } = await loopTask(f, runtime);
    let calls = 0;
    const provider = () => { calls++; return { text: 'Restart-safe answer' }; };
    await runtime.crashTask(taskId, provider, 'completion-reply');
    const pending = fixtureDatabase(join(f.dir, filename));
    try {
      const row = pending.prepare('SELECT intent_json FROM device_journal WHERE task_id=?').get(taskId)!;
      const intent = JSON.parse(String(row.intent_json));
      assert.equal(typeof intent.completeRequestId, 'string');
      assert.equal(intent.completionReceipt, undefined);
      assert.equal((await f.send(`/api/tasks/${taskId}`)).body.data.state, 'completed');
    } finally { pending.close(); }
    assert.equal((await runtime.runTask(taskId, () => { calls++; return { text: 'WRONG' }; })).state, 'completed');
    assert.equal(calls, 1);
    assert.deepEqual((await f.send(`/api/tasks/${taskId}`)).body.data.results.map((row: { content: unknown }) => row.content), [{ text: 'Restart-safe answer' }]);
    const recovered = fixtureDatabase(join(f.dir, filename));
    try {
      const row = recovered.prepare('SELECT intent_json FROM device_journal WHERE task_id=?').get(taskId)!;
      const intent = JSON.parse(String(row.intent_json));
      assert.equal(intent.completionReceipt.requestId, intent.completeRequestId);
    } finally { recovered.close(); }
  } finally { await f.close(); }
});

test('home execution loop survives SIGKILL across provider and tool results without replaying effects', async () => {
  const f = await fixture(), database = join(f.dir, 'killed-loop.sqlite'), keyring = join(f.dir, 'loop-keys.json');
  const options = { database, keyring, server: f.origin() }, calls = new Map<string, number>();
  const provider = createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    const { snapshot } = JSON.parse(body); calls.set(snapshot.taskId, (calls.get(snapshot.taskId) ?? 0) + 1);
    res.end(JSON.stringify(snapshot.toolJournal.some((row: { kind: string; result: { name?: string } }) => row.result?.name === 'Saved by loop') ? { text: 'Saved profile once' } : { toolCall: { id: 'profile', function: { name: 'edit_profile', arguments: JSON.stringify({ revision: 1, name: 'Saved by loop', role: 'Personal agent', instructions: 'Retain saved changes' }) } } }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve)); const address = provider.address(); if (!address || typeof address === 'string') throw new Error('Missing provider fixture address'); const providerOrigin = `http://127.0.0.1:${address.port}`;
  let child: ReturnType<typeof spawn> | undefined;
  try {
    writeFileSync(keyring, '{}', { mode: 0o600 }); const { deviceId } = await enrollRustDevice(options, f.jwt());
    async function launch(taskId: string, checkpoint?: string) {
      child = startRustDeviceTask(options, taskId, providerOrigin, checkpoint);
      let errors = '';
      child.stderr!.on('data', chunk => { errors += chunk; });
      const lines = createInterface({ input: child.stdout! });
      const exited = once(child, 'close');
      const message = await Promise.race([
        once(lines, 'line').then(([line]) => JSON.parse(line)),
        exited.then(() => { throw new Error(`Rust task host exited: ${errors}`); }),
      ]);
      lines.close();
      return { child, exited, message };
    }
    for (const checkpoint of ['provider-not-sent', 'canonical-not-sent', 'provider-saved', 'tool-reply', 'tool-report', 'provider-in-flight']) {
      const { taskId } = await loopTask(f, { deviceId: () => deviceId }), running = await launch(taskId, checkpoint); assert.equal(running.message.checkpoint, checkpoint);
      running.child.kill('SIGKILL'); assert.equal((await running.exited)[1], 'SIGKILL'); await f.restart();
      for (let restart = 0; restart < 2; restart++) {
        const recovered = await launch(taskId); assert.equal(recovered.message.state, checkpoint === 'provider-in-flight' ? 'uncertain' : 'completed'); assert.equal((await recovered.exited)[0], 0);
        assert.equal(calls.get(taskId), checkpoint === 'provider-in-flight' ? 1 : 2);
      }
      const detail = (await f.send(`/api/tasks/${taskId}`)).body.data;
      assert.equal(detail.results.length, checkpoint === 'provider-in-flight' ? 0 : 3);
      if (checkpoint !== 'provider-in-flight') assert.deepEqual(detail.results[1].content, { name: 'Saved by loop', role: 'Personal agent', instructions: 'Retain saved changes', revision: 2 });
    }
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) { const exited = once(child, 'exit'); child.kill('SIGKILL'); await exited; }
    provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); await f.close();
  }
});

test('home execution loop uses explicit uncertainty retry reservation and never retries before that decision', async () => {
  const f = await fixture(), filename = 'risk-loop.sqlite';
  try {
    const runtime = await rustTaskFixture(f, filename);
    const { taskId } = await loopTask(f, runtime); let calls = 0;
    await assert.rejects(runtime.runTask(taskId, () => { calls++; throw new Error('Provider reply lost'); }), /Provider reply lost/);
    assert.equal((await runtime.runTask(taskId, () => { calls++; return { text: 'MUST NOT CALL' }; })).state, 'uncertain'); assert.equal(calls, 1);
    const db = fixtureDatabase(join(f.dir, filename));
    let actionId: string;
    try {
      const recovered = db.prepare("SELECT j.action_id,j.state,o.state AS ack FROM device_journal j JOIN device_outbox o USING(action_id) WHERE j.task_id=?").get(taskId)!;
      assert.equal(recovered.state,'uncertain');assert.equal(recovered.ack,'acknowledged');actionId=String(recovered.action_id);
    } finally { db.close(); }
    const detail = (await f.send(`/api/tasks/${taskId}`)).body.data, requestId = randomUUID();
    const decision = await f.send(`/api/tasks/${taskId}/control`, { requestId, expectedRevision: detail.revision, operation: 'resolve_uncertain', decision: { kind: 'retry_with_risk', actionId, acknowledgement: 'effect_may_have_happened', reason: 'Synthetic provider output lost; retry explicitly approved' } });
    assert.equal(decision.status, 200); assert.equal(decision.body.data.replacementActionId, requestId);
    assert.equal((await runtime.runTask(taskId, () => { calls++; return { text: 'Approved replacement answer' }; })).state, 'completed'); assert.equal(calls, 2);
    const completed = (await f.send(`/api/tasks/${taskId}`)).body.data; assert.equal(completed.results[0].actionId, requestId); assert.deepEqual(completed.results[0].content, { text: 'Approved replacement answer' });
  } finally { await f.close(); }
});

test('home execution loop consumes resolved late-known output without repeating its provider call', async () => {
  const f = await fixture(), database = join(f.dir, 'known-loop.sqlite'), runtime = f.device(database), originalFetch = globalThis.fetch;
  try {
    await runtime.enroll(f.jwt()); const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
    const { taskId } = await loopTask(f, runtime); let calls = 0, prepared: Record<string, any> = {}, lastReport: Record<string, any> = {};
    globalThis.fetch = async (url, init) => { const path = new URL(String(url)).pathname; if (path.endsWith('/prepare')) prepared = { ...JSON.parse(String(init!.body)), actionId: path.split('/').at(-2) }; if (path.endsWith('/reconcile')) lastReport = JSON.parse(String(init!.body)); return originalFetch(url, init); };
    await assert.rejects(runtime.runTask(taskId, () => { calls++; throw new Error('Reply lost'); }), /Reply lost/); await runtime.recoverPending();
    const resultContent = { toolCall: { id: 'settings', function: { name: 'read_provider_settings', arguments: '{}' } } }, encoded = canonicalJson(resultContent), resultRef = { v: 1, taskId, actionId: prepared.actionId, digest: sha256(encoded), size: Buffer.byteLength(encoded), encoding: 'canonical-json-v1' }, evidenceRequestId = randomUUID();
    await runtime.reconcileTask(taskId, { requestId: evidenceRequestId, actionId: prepared.actionId, ownership: prepared.ownership, payloadDigest: prepared.payloadDigest, state: 'reported', effectStatus: 'known', journalSequence: lastReport.journalSequence + 1, resultDigest: resultRef.digest, resultRef, resultContent });
    const detail = (await f.send(`/api/tasks/${taskId}`)).body.data;
    assert.equal((await f.send(`/api/tasks/${taskId}/control`, { requestId: randomUUID(), expectedRevision: detail.revision, operation: 'resolve_uncertain', decision: { kind: 'known_result', actionId: prepared.actionId, evidenceRequestId, resultRef } })).status, 200);
    globalThis.fetch = originalFetch;
    assert.equal((await runtime.runTask(taskId, context => { calls++; assert.match(JSON.stringify(context.snapshot.toolJournal), /configured/); return { text: 'Known tool choice retained' }; })).state, 'completed'); assert.equal(calls, 2);
    assert.equal((await f.send(`/api/tasks/${taskId}`)).body.data.results.length, 3);
    const home = (await f.send('/api/devices')).body.data.find((row: { id: string }) => row.id === runtime.deviceId());
    assert.equal((await f.send(`/api/devices/${home.id}/revoke`, { requestId: randomUUID(), expectedRevision: home.revision })).status, 200);
    await connection.stop(); await runtime.close();
    const recovery = await f.send(`/api/tasks/${taskId}/recovery-session`, { requestId: randomUUID(), expectedAuthorityGeneration: prepared.ownership.authorityGeneration, sourceDeviceId: home.id, sourceDeviceGeneration: prepared.ownership.deviceGeneration });
    assert.equal(recovery.status, 201);
    const exported = exportRustDeviceRecovery(runtime.options, taskId, [String(prepared.actionId)], randomUUID(), recovery.body.data.recoveryGeneration, true);
    const response = await fetch(f.origin() + `/api/tasks/${taskId}/recovery-results`, { method: 'POST', headers: { Authorization: `Bearer ${f.jwt()}`, Origin: f.origin(), 'Content-Type': 'application/json', 'X-Minarbot-Protocol': '1', 'X-Minarbot-Request-Id': exported.requestId, 'X-Minarbot-Recovery': recovery.body.data.session }, body: JSON.stringify(exported) });
    assert.equal(response.status, 200, await response.text());
    assert.equal(exported.envelope.entries[0].effectStatus, 'known');
    assert.deepEqual(exported.envelope.entries[0].resultRef, resultRef); assert.deepEqual(exported.resultContent, resultContent);
    assert.equal(calls, 2);
  } finally { globalThis.fetch = originalFetch; await runtime.close(); await f.close(); }
});

test('home execution loop persists provider wait across reply loss and preserves concurrent pause', async () => {
  const f = await fixture(), proxy = await startDeviceFaultProxy(f.origin);
  try {
    const runtime = await rustTaskFixture(f, 'wait-loop.sqlite', proxy.origin);
    const missing = await loopTask(f, runtime, false, null);
    let waitStatus: number | undefined;
    proxy.fault({ path: '/wait-provider', drop: status => { waitStatus = status; return true; } });
    await assert.rejects(runtime.runTask(missing.taskId, () => assert.fail('No provider binding')), { code: 'device_offline' });
    assert.equal(waitStatus, 200, 'Lost reply must follow a durable provider wait');
    proxy.fault();
    const waiting = (await f.send('/api/tasks/' + missing.taskId)).body.data;
    assert.equal(waiting.state, 'waiting_resource');
    assert.equal((await runtime.runTask(missing.taskId, () => assert.fail('No provider binding'))).state, 'waiting_resource');
    assert.equal((await f.send('/api/tasks/' + missing.taskId)).body.data.revision, waiting.revision);

    const paused = await loopTask(f, runtime, false, null);
    const path = '/api/tasks/' + paused.taskId;
    proxy.fault({ path: path + '/wait-provider', before: async () => {
      const detail = (await f.send(path)).body.data;
      const response = await f.send(path + '/control', { requestId: randomUUID(), expectedRevision: detail.revision, operation: 'pause' });
      assert.equal(response.status, 200);
    } });
    await assert.rejects(runtime.runTask(paused.taskId, () => assert.fail('Paused task must not dispatch')), { code: 'stale_revision' });
    assert.equal((await f.send(path)).body.data.state, 'paused');
  } finally { await proxy.close(); await f.close(); }
});

test('home execution loop holds missing bindings and enforces provider slots before dispatch', async () => {
  const f = await fixture(), runtime = f.device(join(f.dir, 'capacity-loop.sqlite'));
  const releases: (() => void)[] = [], running: Promise<unknown>[] = [];
  const hold = () => new Promise<unknown>(resolve => { releases.push(() => resolve({ text: 'Released answer' })); });
  try {
    await runtime.enroll(f.jwt()); const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
    const missing = await loopTask(f, runtime, false, null);
    assert.equal((await runtime.runTask(missing.taskId, () => assert.fail('Missing binding must not dispatch'))).state, 'waiting_resource');
    assert.equal((await f.send(`/api/tasks/${missing.taskId}`)).body.data.state, 'waiting_resource');
    const first = await loopTask(f, runtime); running.push(runtime.runTask(first.taskId, hold)); await until(() => releases.length === 1);
    await assert.rejects(runtime.runTask(first.taskId, hold), (error: unknown) => error instanceof ProtocolError && error.code === 'executor_busy');
    const same = randomUUID(), payload = { v: 1, kind: 'direct', message: { text: 'Same agent/project', files: [] } }, expectedRevisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 };
    assert.equal((await f.send('/api/tasks', { requestId: same, projectId: first.project, agentId: first.agent, homeDeviceId: runtime.deviceId(), payload, expectedRevisions, payloadDigest: digestCommand(['minarbot-task-payload-v1', same, first.project, first.agent, runtime.deviceId(), payload, expectedRevisions]) })).status, 202);
    assert.equal((await runtime.runTask(same, () => assert.fail('Same agent/project must wait'))).state, 'waiting_capacity');
    const second = await loopTask(f, runtime, false, first.binding); running.push(runtime.runTask(second.taskId, hold)); await until(() => releases.length === 2);
    const third = await loopTask(f, runtime, false, first.binding); assert.equal((await runtime.runTask(third.taskId, () => assert.fail('Third binding call must wait'))).state, 'waiting_capacity');
    for (let count = 2; count < 4; count++) { const task = await loopTask(f, runtime); running.push(runtime.runTask(task.taskId, hold)); await until(() => releases.length === count + 1); }
    const fifth = await loopTask(f, runtime); assert.equal((await runtime.runTask(fifth.taskId, () => assert.fail('Fifth host call must wait'))).state, 'waiting_capacity');
    // Refill the real 40-proof/2s device budget before releasing four held calls.
    await new Promise(resolve => setTimeout(resolve, 2000));
    releases.forEach(release => release()); await Promise.all(running);
    assert.equal((await runtime.runTask(same, () => ({ text: 'Later same-agent answer' }))).state, 'completed');
  } finally { releases.forEach(release => release()); await Promise.allSettled(running); await runtime.close(); await f.close(); }
});

for (const lostAt of ['request', 'reply', 'receipt', 'revoked', 'storage'] as const) test(`canonical completion recovery handles ${lostAt} without another provider call`, async () => {
  const f = await fixture(), database = join(f.dir, 'completion-home.sqlite');
  let runtime = f.device(database);
  const originalFetch = globalThis.fetch;
  let calls = 0;
  try {
    await runtime.enroll(f.jwt());
    const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
    const home = (await f.send('/api/devices')).body.data[0], project = randomUUID(), agent = randomUUID(), binding = randomUUID();
    assert.equal((await f.send('/api/projects', { requestId: randomUUID(), id: project, name: 'Completion' })).status, 201);
    const seed = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
    try { seed.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','synthetic-completion','Synthetic','ready',1)").run(binding, home.id, seed.prepare('SELECT account FROM devices WHERE id=?').get(home.id)!.account); } finally { seed.close(); }
    assert.equal((await f.send('/api/agents', { requestId: agent, id: agent, name: 'Writer', model: 'fixture', backupModel: '', instructions: 'Answer carefully', threadLimit: 2, projectId: project, homeDeviceId: home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: home.revision, providerBinding: { bindingId: binding, expectedRevision: 1 } })).status, 201);
    const taskId = randomUUID(), payload = { v: 1, kind: 'direct', message: { text: 'Answer once', files: [] } }, expectedRevisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 };
    { const admitted = await f.send('/api/tasks', { requestId: taskId, projectId: project, agentId: agent, homeDeviceId: home.id, payload, expectedRevisions, payloadDigest: digestCommand(['minarbot-task-payload-v1', taskId, project, agent, home.id, payload, expectedRevisions]) }); assert.equal(admitted.status, 202, JSON.stringify(admitted.body)); }
    const owned = await runtime.startTask(taskId), context = await runtime.taskContext(taskId, owned.ownership, owned.revision), actionId = randomUUID();
    await runtime.executeAction({ taskId, actionId, sequence: 0, actionKind: 'provider', ownership: owned.ownership, payload: { purpose: 'answer' }, context, grantRevision: 1, bindingId: binding, bindingRevision: 1 }, () => { calls++; return { text: 'Saved final answer' }; });
    globalThis.fetch = async (url, init) => {
      if (String(url).endsWith('/complete') && ['request', 'revoked', 'storage'].includes(lostAt)) throw new Error('Lost completion request');
      const response = await originalFetch(url, init);
      if (String(url).endsWith('/complete')) {
        assert.equal(response.status, 200, await response.clone().text());
        if (lostAt === 'receipt') { const body = await response.json(); body.receipt.payloadDigest = '0'.repeat(64); return Response.json(body); }
        await response.body?.cancel(); throw new Error('Lost completion reply');
      }
      return response;
    };
    await assert.rejects(runtime.completeTask(taskId, actionId), lostAt === 'receipt' ? (error: unknown) => error instanceof ProtocolError && error.code === 'receipt_mismatch' : /Lost completion/);
    globalThis.fetch = originalFetch;
    if (lostAt === 'revoked' || lostAt === 'storage') {
      const db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
      try { db.prepare('UPDATE agent_projects SET deleted=1 WHERE project=? AND agent=?').run(project, agent); } finally { db.close(); }
    }
    await runtime.close();
    // restart() waits for devices the server still lists as ready; let the closed host's disconnect land first.
    await until(async () => (await f.send('/api/devices')).body.data.every((device: { status: string }) => device.status !== 'ready'), deviceReconnectTimeout);
    await f.restart();
    if (lostAt === 'storage') {
      const db = testDatabase(database);
      try { db.exec("CREATE TRIGGER fail_recovery_marker BEFORE UPDATE OF intent_json ON device_journal WHEN json_extract(NEW.intent_json,'$.completionError') IS NOT NULL BEGIN SELECT RAISE(ABORT,'completion marker write failed'); END"); } finally { db.close(); }
    }
    runtime = f.device(database);
    const reconnected = runtime.connect(); await until(() => reconnected.state().phase === 'connected');
    await until(() => runtime.recoveryStatus().results.some(row => row.actionId === actionId && (lostAt === 'storage' ? row.error === 'recovery_storage_unavailable' : lostAt === 'revoked' ? row.error === 'execution_denied' : row.state === 'completed')));
    if (lostAt === 'storage') {
      const db = testDatabase(database);
      try { db.exec('DROP TRIGGER fail_recovery_marker'); } finally { db.close(); }
      assert.deepEqual(await runtime.recoverPending(), [{ taskId, actionId, error: 'execution_denied' }]);
    }
    if (lostAt === 'revoked' || lostAt === 'storage') {
      assert.deepEqual(await runtime.recoverPending(), []);
      await runtime.close();
      runtime = f.device(database);
      const restarted = runtime.connect(); await until(() => restarted.state().phase === 'connected');
      assert.deepEqual(await runtime.recoverPending(), []);
      const db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
      try {
        assert.equal(db.prepare('SELECT reply FROM turns WHERE id=?').get(taskId)!.reply, null);
        db.prepare('UPDATE agent_projects SET deleted=0 WHERE project=? AND agent=?').run(project, agent);
      } finally { db.close(); }
    }
    const completed = await runtime.completeTask(taskId, actionId);
    assert.equal(completed.data.state, 'completed');
    assert.deepEqual((await runtime.completeTask(taskId, actionId)).receipt, completed.receipt);
    const detail = (await f.send(`/api/tasks/${taskId}`)).body.data;
    assert.equal(detail.state, 'completed'); assert.equal(detail.results.length, 1); assert.equal(calls, 1);
    assert.deepEqual(await runtime.recoverPending(), []);
    await runtime.close();
    runtime = f.device(database);
    const restarted = runtime.connect(); await until(() => restarted.state().phase === 'connected');
    assert.deepEqual(await runtime.recoverPending(), []);
    const inspect = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
    try {
      const row = inspect.prepare('SELECT status,reply,paused FROM turns WHERE id=?').get(taskId)!;
      assert.deepEqual({ ...row }, { status: 'done', reply: 'Saved final answer', paused: 0 });
      assert.equal(inspect.prepare('SELECT count(*) n FROM receipts WHERE scope=?').get(`tasks:complete:${taskId}`)!.n, 1);
    } finally { inspect.close(); }
    assert.equal((await f.send(`/api/tasks/${taskId}/control`, { requestId: randomUUID(), expectedRevision: detail.revision, operation: 'resume' })).status, 409);
  } finally { globalThis.fetch = originalFetch; await runtime.close(); await f.close(); }
});

for (const scenario of ['pause', 'cancel', 'latest', 'invalid', 'setup', 'revoked'] as const) test(`canonical completion enforces ${scenario} through signed HTTP`, async () => {
  const f = await fixture(), runtime = f.device(join(f.dir, 'completion-controls.sqlite'));
  const protocolCode = (expected: string) => (error: unknown) => error instanceof ProtocolError && error.code === expected;
  try {
    await runtime.enroll(f.jwt()); const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
    const home = (await f.send('/api/devices')).body.data[0], project = randomUUID(), binding = randomUUID(), agent = randomUUID(), taskId = scenario === 'setup' ? agent : randomUUID();
    assert.equal((await f.send('/api/projects', { requestId: randomUUID(), id: project, name: 'Completion controls' })).status, 201);
    const db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
    try {
      db.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','completion-controls','Synthetic','ready',1)").run(binding, home.id, db.prepare('SELECT account FROM devices WHERE id=?').get(home.id)!.account);
      assert.equal((await f.send('/api/agents', { requestId: agent, id: agent, name: 'Writer', model: 'fixture', backupModel: '', instructions: 'Answer carefully', threadLimit: 2, projectId: project, homeDeviceId: home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: home.revision, providerBinding: { bindingId: binding, expectedRevision: 1 }, setup: scenario === 'setup' })).status, 201);
      if (scenario !== 'setup') {
        const payload = { v: 1, kind: 'direct', message: { text: 'Answer once', files: [] } }, expectedRevisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 };
        { const admitted = await f.send('/api/tasks', { requestId: taskId, projectId: project, agentId: agent, homeDeviceId: home.id, payload, expectedRevisions, payloadDigest: digestCommand(['minarbot-task-payload-v1', taskId, project, agent, home.id, payload, expectedRevisions]) }); assert.equal(admitted.status, 202, JSON.stringify(admitted.body)); }
      }
      const owned = await runtime.startTask(taskId); let sequence = 0;
      const run = async (content: unknown, canonical = false) => {
        const detail = (await f.send(`/api/tasks/${taskId}`)).body.data, context = await runtime.taskContext(taskId, owned.ownership, detail.revision), actionId = randomUUID();
        await runtime.executeAction({ taskId, actionId, sequence: sequence++, actionKind: canonical ? 'canonical' : 'provider', ownership: owned.ownership, payload: canonical ? { toolCall: { id: actionId, function: { name: 'edit_profile', arguments: JSON.stringify(content) } }, expectedRevision: context.snapshot.taskRevision + 1 } : { purpose: 'answer' }, context, grantRevision: 1, ...(!canonical ? { bindingId: binding, bindingRevision: 1 } : {}) }, () => { assert(!canonical); return content; });
        return actionId;
      };
      const actionId = await run(scenario === 'invalid' ? { text: ' ', extra: true } : { text: 'Saved answer' });
      if (scenario === 'pause' || scenario === 'cancel') {
        const control = async (operation: string) => {
          const detail = (await f.send(`/api/tasks/${taskId}`)).body.data;
          assert.equal((await f.send(`/api/tasks/${taskId}/control`, { requestId: randomUUID(), expectedRevision: detail.revision, operation })).status, 200);
        };
        await control(scenario); await assert.rejects(runtime.completeTask(taskId, actionId), protocolCode(scenario === 'pause' ? 'task_not_runnable' : 'ownership_conflict'));
        assert.equal(db.prepare('SELECT reply FROM turns WHERE id=?').get(taskId)!.reply, null);
        if (scenario === 'pause') { await control('resume'); await runtime.completeTask(taskId, actionId); }
      } else if (scenario === 'invalid') await assert.rejects(runtime.completeTask(taskId, actionId), protocolCode('result_invalid'));
      else if (scenario === 'revoked') {
        db.prepare('UPDATE agent_projects SET deleted=1 WHERE project=? AND agent=?').run(project, agent);
        await assert.rejects(runtime.completeTask(taskId, actionId), protocolCode('execution_denied'));
        db.prepare('UPDATE agent_projects SET deleted=0 WHERE project=? AND agent=?').run(project, agent);
        const completed = await runtime.completeTask(taskId, actionId);
        db.prepare('UPDATE agent_projects SET deleted=1 WHERE project=? AND agent=?').run(project, agent);
        await assert.rejects(runtime.completeTask(taskId, actionId), protocolCode('execution_denied'));
        assert.equal(db.prepare('SELECT state,revision FROM tasks WHERE id=?').get(taskId)!.revision, completed.data.revision);
        assert.equal(db.prepare('SELECT reply FROM turns WHERE id=?').get(taskId)!.reply, 'Saved answer');
      }
      else if (scenario === 'setup') {
        await assert.rejects(runtime.completeTask(taskId, actionId), protocolCode('setup_incomplete'));
        await run({ revision: 1, name: 'Research partner', instructions: 'Cite evidence' }, true);
        await assert.rejects(runtime.completeTask(taskId, actionId), protocolCode('outcome_unresolved'));
        await runtime.completeTask(taskId, await run({ text: 'Profile saved' }));
        assert.equal(db.prepare('SELECT status FROM turns WHERE id=?').get(taskId)!.status, 'done');
      } else {
        const latest = await run({ text: 'Latest answer' }); await assert.rejects(runtime.completeTask(taskId, actionId), protocolCode('outcome_unresolved'));
        db.exec("CREATE TRIGGER fail_completion BEFORE UPDATE OF reply ON turns BEGIN SELECT RAISE(ABORT,'completion write failed'); END");
        await assert.rejects(runtime.completeTask(taskId, latest));
        assert.equal(db.prepare('SELECT count(*) n FROM receipts WHERE scope=?').get(`tasks:complete:${taskId}`)!.n, 0);
        assert.equal(db.prepare('SELECT state FROM tasks WHERE id=?').get(taskId)!.state, 'running');
        db.exec('DROP TRIGGER fail_completion'); await runtime.completeTask(taskId, latest);
        assert.equal(db.prepare('SELECT reply FROM turns WHERE id=?').get(taskId)!.reply, 'Latest answer');
      }
    } finally { db.close(); }
  } finally { await runtime.close(); await f.close(); }
});

test('canonical listener rejects a second server on the same database and releases lock on close', async () => {
  const f=await fixture();let duplicate:Awaited<ReturnType<typeof startCanonicalApp>>|undefined;
  try {
    await assert.rejects(async()=>{duplicate=await f.startAgain();},(error:unknown)=>error instanceof ProtocolError&&error.code==='canonical_server_busy');
    const alias=join(f.dir,'alias.sqlite');symlinkSync(join(f.dir,'canonical.sqlite'),alias);
    await assert.rejects(async()=>{duplicate=await f.startAgain(alias);},(error:unknown)=>error instanceof ProtocolError&&error.code==='canonical_server_busy');
    assert.equal((await f.send('/api/session')).status,200);
    const failed=join(f.dir,'failed.sqlite');await assert.rejects(f.startAgain(failed,Number(new URL(f.origin()).port)),{code:'EADDRINUSE'});
    duplicate=await f.startAgain(failed);await duplicate.close();duplicate=undefined;
    await f.restart();assert.equal((await f.send('/api/session')).status,200);
  } finally { await duplicate?.close();await f.close(); }
});

test('cross-device reassign survives lost reply and restart, preserves old results and waits for fixed recipient home', async () => {
  const f = await fixture();
  const first = f.device(join(f.dir,'a.sqlite'));
  const second = f.device(join(f.dir,'b.sqlite'));
  const originalFetch = globalThis.fetch;
  let effects = 0;
  try {
    await first.enroll(f.jwt()); await second.enroll(f.jwt());
    const connection = first.connect(); await until(() => connection.state().phase === 'connected');
    const homes = (await f.send('/api/devices')).body.data, project = randomUUID(), source = randomUUID(), recipient = randomUUID();
    assert.equal((await f.send('/api/projects',{requestId:randomUUID(),id:project,name:'Transfer'})).status,201);
    for (const [agentId,homeId] of [[source,first.deviceId()],[recipient,second.deviceId()]]) {
      assert.equal((await f.send('/api/agents',{requestId:agentId,id:agentId,name:agentId,model:'fixture',backupModel:'',instructions:'Continue safely',threadLimit:2,projectId:project,homeDeviceId:homeId,expectedProjectRevision:1,expectedHomeDeviceRevision:homes.find((home:{id:string})=>home.id===homeId).revision,providerBinding:null})).status,201);
    }
    const taskId = randomUUID(), payload = {v:1,kind:'direct',message:{text:'Keep earlier evidence',files:[]}}, expectedRevisions = {project:1,agent:1,homeBinding:1,membership:1,assignment:1,capability:1};
    const command = {requestId:taskId,projectId:project,agentId:source,homeDeviceId:first.deviceId(),payload,expectedRevisions,payloadDigest:digestCommand(['minarbot-task-payload-v1',taskId,project,source,first.deviceId(),payload,expectedRevisions])};
    const accepted = await f.send('/api/tasks',command); assert.equal(accepted.status,202);
    const owned = await first.startTask(taskId), context = await first.taskContext(taskId,owned.ownership,owned.revision);
    await first.executeAction({taskId,actionId:randomUUID(),sequence:0,actionKind:'read',ownership:owned.ownership,payload:{operation:'original'},context,grantRevision:1},()=>{effects++;return{text:'Saved source output'};});
    const detail = (await f.send(`/api/tasks/${taskId}`)).body.data;
    const paused = await f.send(`/api/tasks/${taskId}/control`,{requestId:randomUUID(),expectedRevision:detail.revision,operation:'pause'}); assert.equal(paused.status,200);
    const reassign = {requestId:randomUUID(),expectedRevision:paused.body.data.revision,operation:'reassign',recipientAgentId:recipient};
    globalThis.fetch = async (url,init) => { const response = await originalFetch(url,init); if(String(url).endsWith(`/api/tasks/${taskId}/control`)){await response.body?.cancel();throw new Error('Lost transfer ACK');} return response; };
    await assert.rejects(f.send(`/api/tasks/${taskId}/control`,reassign),/Lost transfer ACK/); globalThis.fetch = originalFetch;
    await f.restart();
    const retry = await f.send(`/api/tasks/${taskId}/control`,reassign); assert.equal(retry.status,200,JSON.stringify(retry.body));
    const nextId = retry.body.data.taskId; assert.notEqual(nextId,taskId); assert.equal(retry.body.data.homeDeviceId,second.deviceId());
    const original = await f.send(`/api/tasks/${taskId}`), next = await f.send(`/api/tasks/${nextId}`);
    assert.equal(original.body.data.currentTaskId,nextId); assert.equal(original.body.data.state,'transferred');
    assert.deepEqual(original.body.receipt,accepted.body.receipt);
    assert.equal(next.body.data.state,'waiting_device'); assert.equal(next.body.data.agentId,recipient);
    assert.equal((await f.send(`/api/tasks/${taskId}/control`,{...reassign,requestId:randomUUID()})).body.error.code,'task_transferred');
    const nextConnection = second.connect(); await until(()=>nextConnection.state().phase==='connected');
    const nextOwner = await second.startTask(nextId), continued = await second.taskContext(nextId,nextOwner.ownership,nextOwner.revision);
    assert.equal((continued.snapshot.toolJournal as {result:{text:string}}[])[0].result.text,'Saved source output');
    await first.recoverPending(); assert.equal(effects,1);
    assert.equal((await f.send('/api/tasks',command)).body.data.taskId,taskId);
  } finally { globalThis.fetch=originalFetch; await first.close(); await second.close(); await f.close(); }
});

test('local possession gate enrolls only configured server and retains identity across reconnect/revoke', async () => {
  const f = await fixture(), database = join(f.dir, 'device.sqlite'), keyring = join(f.dir, 'device-keys.json');
  writeFileSync(keyring, '{}', { mode: 0o600 });
  const config = () => ({ database, server: f.origin(), keyring });
  let link: Awaited<ReturnType<typeof startRustDeviceEnrollment>> | undefined;
  let connection: ReturnType<typeof startRustDeviceConnection> | undefined;
  try {
    const listener = link = await startRustDeviceEnrollment(config()), token = readFileSync(listener.tokenFile, 'utf8');
    assert.equal(statSync(listener.tokenFile).mode & 0o777, 0o600);
    const local = async (headers = {}, path = '/local/link-proof', body: unknown = { requestId: randomUUID(), clerkToken: f.jwt() }) => {
      const response = await fetch(listener.origin + path, { method: 'POST', headers: { Origin: listener.origin, 'Content-Type': 'application/json', Authorization: `Bearer ${token}`, ...headers }, body: JSON.stringify(body) }); return { status: response.status, body: await response.json() };
    };
    assert.equal((await local({ Origin: 'https://evil.example' })).status, 403);
    assert.equal((await local({ Authorization: 'Bearer wrong' })).status, 403);
    assert.equal((await local({ 'X-Forwarded-Host': 'evil.example' })).status, 403);
    assert.equal((await local({}, '/local/sign-arbitrary')).status, 403);
    const linked = await local(); assert.equal(linked.status, 200, JSON.stringify(linked.body));
    await listener.wait(); assert(!existsSync(listener.tokenFile));
    const deviceId = linked.body.data.deviceId;
    connection = startRustDeviceConnection(config());
    await connection.ready();
    const devices = (await f.send('/api/devices')).body.data; assert.equal(devices.length, 1); assert.equal(devices[0].id, deviceId); assert.equal(devices[0].status, 'ready');
    assert.equal((await f.send('/api/devices', undefined, 'user_foreign')).body.data.length, 0);
    const project = randomUUID(), createProject = { requestId: randomUUID(), id: project, name: 'A' };
    assert.equal((await f.send('/api/projects', createProject)).status, 201);
    const id = randomUUID(), createAgent = { requestId: id, id, name: 'Remote', model: 'fixture', backupModel: '', instructions: 'Help with research', threadLimit: 2, projectId: project, homeDeviceId: deviceId, expectedProjectRevision: 1, expectedHomeDeviceRevision: devices[0].revision, providerBinding: null, setup: false };
    assert.equal((await f.send('/api/agents', createAgent)).status, 201);
    assert.equal((await f.send('/api/agents', createAgent)).status, 200);
    const avatar = await sharp(randomBytes(256 * 256 * 3), { raw: { width: 256, height: 256, channels: 3 } }).png().toBuffer();
    const avatarId = randomUUID(), avatarCommand = { ...createAgent, requestId: avatarId, id: avatarId, avatar: avatar.toString('base64') };
    assert(Buffer.byteLength(JSON.stringify(avatarCommand)) > 65536);
    assert.equal((await f.send('/api/agents', avatarCommand)).status, 201);
    assert.equal((await f.send('/api/agents', { ...avatarCommand, avatar: 'a'.repeat(750000) })).status, 413);

    assert.equal((await f.send(`/api/projects/${project}/agents/${id}/rules`, { requestId: randomUUID(), expectedParticipationRevision: 1, rules: 'A only' }, 'user_owner', 'PUT')).status, 200);
    assert.equal((await f.send(`/api/projects/${project}/agents/${id}/capabilities`, { requestId: randomUUID(), expectedRevision: 2, capabilities: { v: 1, tools: [], resources: [] } }, 'user_owner', 'PUT')).status, 200);
    await connection.stop();
    await f.restart();
    const resumed = connection = startRustDeviceConnection(config()); await resumed.ready();
    assert.equal((await f.send('/api/devices')).body.data[0].id, deviceId);
    const current = (await f.send('/api/devices')).body.data[0];
    assert.equal((await f.send(`/api/devices/${deviceId}/revoke`, { requestId: randomUUID(), expectedRevision: current.revision })).status, 200);
    await until(() => resumed.state()?.phase === 'blocked'); await resumed.wait();
    const canonical = fixtureDatabase(join(f.dir, 'canonical.sqlite')), device = testDatabase(database);
    try {
      assert.equal(device.prepare("SELECT name FROM sqlite_schema WHERE name='accounts'").get(), undefined);
      assert.equal(canonical.prepare("SELECT name FROM sqlite_schema WHERE name='device_state'").get(), undefined);
      assert.equal(canonical.prepare('SELECT count(*) AS n FROM device_sessions WHERE revoked=0').get()!.n, 0);
      const publicMetadata = readFileSync(`${database}.host/identity.json`, 'utf8');
      for (const secret of Object.values(JSON.parse(readFileSync(keyring, 'utf8')))) assert(!publicMetadata.includes(String(secret)));
    } finally { canonical.close(); device.close(); }
  } finally { await connection?.stop(); await link?.close(); await f.close(); }
});

test('device key loss fails closed and concurrent stream replacement fences old heartbeat', async () => {
  const f = await fixture(), database = join(f.dir, 'device.sqlite'), keyring = join(f.dir, 'device-keys.json');
  const config = { database, server: f.origin(), keyring };
  const connections: ReturnType<typeof startRustDeviceConnection>[] = [];
  writeFileSync(keyring, '{}', { mode: 0o600 });
  try {
    await enrollRustDevice(config, f.jwt());
    const empty = join(f.dir, 'empty-keys.json');
    writeFileSync(empty, '{}', { mode: 0o600 });
    const missing = startRustDeviceConnection({ ...config, keyring: empty }); connections.push(missing);
    await assert.rejects(missing.wait(), /Device key is unavailable/);
    assert.equal(readFileSync(empty, 'utf8'), '{}', 'Must not replace missing identity key');
    const first = startRustDeviceConnection(config); connections.push(first);
    await first.ready();
    const second = startRustDeviceConnection(config); connections.push(second);
    await second.ready();
    await until(() => first.state()?.phase !== 'connected'); await first.stop();
    const current = (await f.send('/api/devices')).body.data[0]; assert.equal(current.status, 'ready'); assert(current.connection_generation >= 2);
  } finally { await Promise.all(connections.map(connection => connection.stop())); await f.close(); }
});

test('lost enrollment ACK resumes same request and explicit reenroll/rotation preserve home identity', async () => {
  const f = await fixture(), database = join(f.dir, 'device.sqlite');
  let runtime = f.device(database);
  const originalFetch = globalThis.fetch; let drop = true;
  try {
    globalThis.fetch = async (input, init) => {
      const response = await originalFetch(input, init);
      if (drop && String(input) === f.origin() + '/api/devices/link') { drop = false; await response.body?.cancel(); throw new Error('Synthetic lost enrollment ACK'); }
      return response;
    };
    await assert.rejects(runtime.enroll(f.jwt()), /lost enrollment ACK/);
    const pending = JSON.parse(readFileSync(`${database}.host/identity.json`, 'utf8')).pending;
    const linked = await runtime.enroll(f.jwt()); assert.equal(linked.generation, 1);
    const canonical = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
    try {
      assert.equal(canonical.prepare('SELECT count(*) AS n FROM devices').get()!.n, 1);
      assert.equal(canonical.prepare("SELECT count(*) AS n FROM receipts WHERE scope='device_enrollment'").get()!.n, 1);
      assert.equal(canonical.prepare('SELECT request_id FROM receipts WHERE request_id=?').get(pending.request.requestId)!.request_id, pending.request.requestId);
    } finally { canonical.close(); }
    const home = runtime.deviceId();
    const reenrolled = await runtime.enroll(f.jwt(), 'reenroll'); assert.equal(reenrolled.deviceId, home); assert.equal(reenrolled.generation, 2);
    const before = JSON.parse(readFileSync(`${database}.host/identity.json`, 'utf8'));
    const rotated = await runtime.enroll(f.jwt(), 'rotate'); assert.equal(rotated.deviceId, home); assert.equal(rotated.generation, 3);
    const after = JSON.parse(readFileSync(`${database}.host/identity.json`, 'utf8')); assert.notEqual(after.signingRef, before.signingRef); assert.equal(after.descriptor.keyRevision, 2);
    assert([...runtime.keys.values.keys()].some(key => key.endsWith('/' + before.signingRef)));
    await runtime.close(); runtime = f.device(database);
    const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
  } finally { globalThis.fetch = originalFetch; await runtime.close(); await f.close(); }
});

test('canonical task HTTP survives lost acceptance response/restart without dispatch and hides private task', async () => {
  const f = await fixture(), database = join(f.dir, 'device.sqlite'), keyring = join(f.dir, 'keys.json');
  try {
    writeFileSync(keyring, '{}', { mode: 0o600 });
    await enrollRustDevice({ database, keyring, server: f.origin() }, f.jwt());
    const home = (await f.send('/api/devices')).body.data[0], project = randomUUID(), agent = randomUUID();
    assert.equal((await f.send('/api/projects', { requestId: randomUUID(), id: project, name: 'Task project' })).status, 201);
    assert.equal((await f.send('/api/agents', { requestId: agent, id: agent, name: 'Agent', model: 'fixture', backupModel: '', instructions: 'Research', threadLimit: 2, projectId: project, homeDeviceId: home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: home.revision, providerBinding: null })).status, 201);
    const input = { requestId: randomUUID(), projectId: project, agentId: agent, homeDeviceId: home.id, payload: { v: 1, kind: 'direct', message: { text: 'Persistent task', files: [] } }, expectedRevisions: { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 } };
    const command = { ...input, payloadDigest: digestCommand(['minarbot-task-payload-v1', input.requestId, project, agent, home.id, input.payload, input.expectedRevisions]) };
    assert.equal((await f.send('/api/tasks', command)).status, 202);
    await f.restart();
    const detail = await f.send(`/api/tasks/${command.requestId}`); assert.equal(detail.status, 200); assert.equal(detail.body.data.state, 'waiting_device');
    assert.equal((await f.send('/api/tasks', command)).status, 200);
    assert.equal((await f.send(`/api/tasks/${command.requestId}`, undefined, 'user_foreign')).status, 404);
    const db = fixtureDatabase(join(f.dir, 'canonical.sqlite'));
    try { assert.equal(db.prepare('SELECT count(*) n FROM tasks').get()!.n, 1); assert.equal(db.prepare('SELECT count(*) n FROM task_actions').get()!.n, 0); }
    finally { db.close(); }
  } finally { await f.close(); }
});

test('signed host HTTP admission commits once and result ACK survives canonical restart', async () => {
  const f = await fixture(), database = join(f.dir, 'device.sqlite');
  const runtime = f.device(database);
  const originalFetch = globalThis.fetch;
  try {
    await runtime.enroll(f.jwt()); const connection = runtime.connect(); await until(() => connection.state().phase === 'connected');
    const home = (await f.send('/api/devices')).body.data[0], project = randomUUID(), agent = randomUUID();
    assert.equal((await f.send('/api/projects', { requestId: randomUUID(), id: project, name: 'Recovery' })).status, 201);
    assert.equal((await f.send('/api/agents', { requestId: agent, id: agent, name: 'Agent', model: 'fixture', backupModel: '', instructions: 'Research', threadLimit: 2, projectId: project, homeDeviceId: home.id, expectedProjectRevision: 1, expectedHomeDeviceRevision: home.revision, providerBinding: null })).status, 201);
    const input = { requestId: randomUUID(), projectId: project, agentId: agent, homeDeviceId: home.id, payload: { v: 1, kind: 'direct', message: { text: 'Persistent result', files: [] } }, expectedRevisions: { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 } };
    const command = { ...input, payloadDigest: digestCommand(['minarbot-task-payload-v1', input.requestId, project, agent, home.id, input.payload, input.expectedRevisions]) };
    assert.equal((await f.send('/api/tasks', command)).status, 202);
    const taskId = input.requestId, acquireId = randomUUID(), lockProof = { bootId: randomUUID(), processId: 1234, processStart: '999', lockId: randomUUID(), previousOwnerStopped: true, reconciledThrough: 0, journalDigest: sha256('journal') };
    let dropAcquire = true;
    globalThis.fetch = async (url, init) => { const response = await originalFetch(url, init); if (dropAcquire && String(url).endsWith('/acquire')) { dropAcquire = false; await response.body?.cancel(); throw new Error('Synthetic lost acquire ACK'); } return response; };
    await assert.rejects(runtime.acquireTask(taskId, lockProof, 0, acquireId), /lost acquire ACK/);
    const acquired = await runtime.acquireTask(taskId, lockProof, 0, acquireId) as Awaited<ReturnType<typeof runtime.startTask>>;
    await assert.rejects(runtime.acquireTask(taskId, lockProof, 0, randomUUID()), error => error instanceof ProtocolError && error.code === 'ownership_conflict');
    globalThis.fetch = originalFetch;
    const { ownership } = acquired, context = await runtime.taskContext(taskId, ownership, acquired.revision);
    assert.equal(context.snapshotRef.digest, sha256(canonicalJson(context.snapshot)));
    const actionId = randomUUID(), payloadDigest = sha256('synthetic read');
    const intentDigest = digestCommand([taskId,actionId,0,ownership,payloadDigest,'read',context.snapshotRef.snapshotId,context.snapshotRef.digest,null,null,1,null,null,null]);
    await runtime.prepareTask(taskId, actionId, { requestId: randomUUID(), sequence: 0, ownership, intentDigest, payloadDigest, actionKind: 'read', contextSnapshotId: context.snapshotRef.snapshotId, contextDigest: context.snapshotRef.digest, grantRevision: 1, expectedTaskRevision: acquired.revision, journalSequence: 1 });
    const commit = { requestId: randomUUID(), ownership, intentDigest, expectedTaskRevision: acquired.revision, journalSequence: 1 };
    assert.equal((await runtime.commitTask(taskId, actionId, commit) as { dispatchable: boolean }).dispatchable, true);
    assert.equal((await runtime.commitTask(taskId, actionId, commit) as { dispatchable: boolean }).dispatchable, false);
    const resultContent = { text: 'Saved over signed HTTP' }, resultDigest = sha256(canonicalJson(resultContent));
    const report = { requestId: randomUUID(), actionId, ownership, payloadDigest, state: 'acknowledged', effectStatus: 'known', journalSequence: 2, resultContent, resultDigest, resultRef: { v: 1, taskId, actionId, digest: resultDigest, size: Buffer.byteLength(canonicalJson(resultContent)), encoding: 'canonical-json-v1' } };
    const published = await runtime.reconcileTask(taskId, report);
    const previousConnectionGeneration = connection.state().generation;
    await f.restart(); await until(() => connection.state().phase === 'connected' && connection.state().generation > previousConnectionGeneration);
    assert.deepEqual(await runtime.reconcileTask(taskId, report), published);
    assert.deepEqual((await f.send(`/api/tasks/${taskId}`)).body.data.results[0].content, resultContent);
    const db = testDatabase(database), canonical = fixtureDatabase(join(f.dir,'canonical.sqlite'));
    try {
      const refs = String(db.prepare('SELECT executor_refs_json FROM device_state WHERE id=1').get()!.executor_refs_json), receipts = JSON.stringify(canonical.prepare('SELECT result_json FROM receipts').all());
      for (const [name, token] of runtime.keys.values) if (name.includes('/executor/')) { assert(!refs.includes(token)); assert(!receipts.includes(token)); }
      assert.equal(canonical.prepare('SELECT count(*) n FROM task_results').get()!.n, 1);
    } finally { db.close(); canonical.close(); }
  } finally { globalThis.fetch = originalFetch; await runtime.close(); await f.close(); }
});

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
